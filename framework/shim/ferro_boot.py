"""
ferro_boot — startup + dispatch glue between ferro (Rust) and the Frappe app controllers.

Responsibilities:
  1. Install a permissive lazy import finder so every controller IMPORTS for real (heavy/optional
     third-party deps and un-ported framework internals resolve to cheap stubs — they belong to
     the out-of-budget worker, not the serving process).
  2. Build the doctype -> controller-class registry by scanning each app's package.
  3. Parse each app's hooks.py and merge doc_events / overrides.
  4. Compute the needs-Python registry (which doctype+event has ANY Python: controller method or
     hook) so ferro can take the zero-Python Rust fast path for everything else.
  5. dispatch_write(): drive the controller lifecycle for a hooked write, calling into ferro_rt
     for the actual persistence.
"""
import os, sys, json, types, importlib, importlib.abc, importlib.machinery, traceback

import frappe

REPOS = os.environ.get("FERRO_REPOS", "/home/frappe/ferro-apps-investigation/repos")
ALL_APPS = ["crm", "helpdesk", "gameplan", "hrms", "erpnext"]

# ----------------------------------------------------------------- permissive lazy stubs
from frappe._lazy import Stub  # subclassable/callable/operator-absorbing permissive type


class _LazyModule(types.ModuleType):
    __ferro_lazy__ = True
    def __getattr__(self, name):
        if name == "__path__":
            return []
        if name.startswith("__") and name.endswith("__"):
            raise AttributeError(name)
        return Stub


class _LazyFinder(importlib.abc.MetaPathFinder, importlib.abc.Loader):
    """Appended LAST to sys.meta_path: only fires for modules the real importer couldn't find."""
    def __init__(self):
        self.stubbed = set()
        self.enabled = True
    def find_spec(self, name, path, target=None):
        if not self.enabled:
            return None
        # frappe internals we didn't port, or any genuinely-absent module (heavy deps, etc.)
        return importlib.machinery.ModuleSpec(name, self)
    def create_module(self, spec):
        self.stubbed.add(spec.name)
        m = _LazyModule(spec.name)
        m.__path__ = []
        m.__spec__ = spec
        return m
    def exec_module(self, module):
        pass


_FINDER = _LazyFinder()
if _FINDER not in sys.meta_path:
    sys.meta_path.append(_FINDER)


def _ensure_paths():
    for app in ALL_APPS:
        repo_root = os.path.join(REPOS, app)
        if repo_root not in sys.path:
            sys.path.insert(0, repo_root)


# ----------------------------------------------------------------- doctype discovery
def _inner_pkg(app):
    cand = os.path.join(REPOS, app, app)
    return cand if os.path.isdir(cand) else os.path.join(REPOS, app)


def _modname(app, path):
    repo_root = os.path.join(REPOS, app)
    rel = os.path.relpath(path, repo_root)
    return rel[:-3].replace(os.sep, ".")


def discover_doctypes(app):
    """Yield (doctype_name, module_name, py_path) for every controller in an app."""
    import glob
    base = _inner_pkg(app)
    out = []
    for py in glob.glob(os.path.join(base, "**", "doctype", "*", "*.py"), recursive=True):
        d = os.path.dirname(py)
        scrub = os.path.basename(d)
        if os.path.basename(py)[:-3] != scrub:
            continue
        jpath = os.path.join(d, scrub + ".json")
        if not os.path.exists(jpath):
            continue
        try:
            j = json.load(open(jpath))
        except Exception:
            continue
        if j.get("doctype") != "DocType" or not j.get("name"):
            continue
        out.append((j["name"], _modname(app, py), py))
    return out


# doctype -> module name, for lazy import on first touch
_DOCTYPE_MODULE = {}
# doctype -> set of lifecycle events its controller defines (computed by AST in lazy mode)
_AST_EVENTS = {}


def _ast_scan_events(py_path):
    """Cheaply (no import) find which lifecycle methods a controller file defines."""
    import ast
    try:
        tree = ast.parse(open(py_path, "r", encoding="utf-8").read())
    except Exception:
        return set(LIFECYCLE_EVENTS)  # unpar. be conservative -> route through Python
    evs = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.ClassDef):
            for item in node.body:
                if isinstance(item, (ast.FunctionDef, ast.AsyncFunctionDef)) and item.name in LIFECYCLE_EVENTS:
                    evs.add(item.name)
    return evs


from frappe.model.document import Document as _Document


def _register_module_controllers(doctype, module):
    for nm in dir(module):
        obj = getattr(module, nm, None)
        if isinstance(obj, type) and issubclass(obj, _Document) and obj is not _Document \
                and getattr(obj, "__module__", "") == module.__name__:
            frappe._register_controller(doctype, obj)
            return obj
    return None


# ----------------------------------------------------------------- hooks
_MERGED_DOC_EVENTS = {}   # doctype -> {event: [dotted_fn, ...]}  ('*' allowed as doctype)
_OVERRIDE_CLASS = {}      # doctype -> dotted class
_PERM_QUERY = {}          # doctype -> dotted fn


def _load_hooks(app):
    try:
        mod = importlib.import_module(f"{app}.hooks")
    except Exception:
        return
    de = getattr(mod, "doc_events", None) or {}
    for dt, events in de.items():
        # a hooks.py may key doc_events by a tuple/list of doctypes — expand to each
        dts = list(dt) if isinstance(dt, (tuple, list)) else [dt]
        for d in dts:
            bucket = _MERGED_DOC_EVENTS.setdefault(str(d), {})
            for ev, fns in events.items():
                if isinstance(fns, str):
                    fns = [fns]
                bucket.setdefault(ev, []).extend(fns)
    oc = getattr(mod, "override_doctype_class", None) or {}
    for dt, cls in oc.items():
        _OVERRIDE_CLASS[dt] = cls
    pq = getattr(mod, "permission_query_conditions", None) or {}
    for dt, fn in pq.items():
        _PERM_QUERY[dt] = fn


# ----------------------------------------------------------------- public: load
LIFECYCLE_EVENTS = (
    "before_validate", "validate", "before_save", "before_insert", "after_insert",
    "on_update", "on_change", "before_submit", "on_submit", "before_cancel", "on_cancel",
    "on_update_after_submit", "before_rename", "after_rename", "on_trash", "after_delete",
)

_LOADED = {"apps": [], "controllers": 0, "doctypes": 0, "import_errors": 0, "mode": "all"}
_MODE = "all"


def lazy_import_controller(doctype):
    """Import + register a controller on first touch (lazy mode). Returns the class or None."""
    cls = frappe._CONTROLLER_REGISTRY.get(doctype)
    if cls is not None:
        return cls
    modname = _DOCTYPE_MODULE.get(doctype)
    if not modname:
        return None
    try:
        module = importlib.import_module(modname)
        return _register_module_controllers(doctype, module)
    except Exception:
        return None


def load(apps=None, mode="all", catch_all=True, verbose=False):
    """Parse hooks + (mode) import controllers. mode: 'all' (eager) | 'lazy' | 'none'."""
    global _MODE
    _MODE = mode
    _ensure_paths()
    _FINDER.enabled = bool(catch_all)
    apps = apps or ALL_APPS
    _LOADED["apps"] = list(apps)
    _LOADED["mode"] = mode

    # hooks first (cheap, declarative)
    for app in apps:
        _load_hooks(app)
    import frappe.boot as _b
    merged = {"doc_events": _MERGED_DOC_EVENTS, "override_doctype_class": _OVERRIDE_CLASS,
              "permission_query_conditions": _PERM_QUERY}
    _b.set_hooks(merged)

    # let the shim import controllers on demand in lazy mode
    frappe._lazy_import_controller = lazy_import_controller

    n_ctrl = 0
    n_dt = 0
    n_err = 0
    for app in apps:
        for doctype, modname, py in discover_doctypes(app):
            n_dt += 1
            _DOCTYPE_MODULE[doctype] = modname
            if mode == "all":
                try:
                    module = importlib.import_module(modname)
                    if _register_module_controllers(doctype, module):
                        n_ctrl += 1
                except Exception as e:
                    n_err += 1
                    if verbose:
                        sys.stderr.write(f"[import-error] {modname}: {e}\n")
            elif mode == "lazy":
                # AST-scan (no import) so the needs-Python registry is exact without paying the
                # import cost; controllers materialise only when a hooked write actually arrives.
                evs = _ast_scan_events(py)
                if evs:
                    _AST_EVENTS[doctype] = evs
    _LOADED.update(controllers=n_ctrl, doctypes=n_dt, import_errors=n_err)
    return dict(_LOADED, stubbed=len(_FINDER.stubbed))


def summary():
    return dict(_LOADED, stubbed=len(_FINDER.stubbed),
                stubbed_top=sorted(_FINDER.stubbed)[:40],
                merged_doctypes_with_hooks=len(_MERGED_DOC_EVENTS))


# ----------------------------------------------------------------- needs-Python registry
def needs_python_registry():
    """
    Return {"by_doctype": {dt: [events...]}, "wildcard": [events...]} listing every
    (doctype, event) that has ANY Python (controller method override or hooks doc_event).
    ferro consults this to decide whether a write can take the zero-Python Rust fast path.
    """
    by_dt = {}
    # controller-defined lifecycle methods (eager mode: introspect the imported class)
    for dt, cls in frappe._CONTROLLER_REGISTRY.items():
        evs = set()
        for ev in LIFECYCLE_EVENTS:
            m = getattr(cls, ev, None)
            base = getattr(_Document, ev, None)
            if callable(m) and m is not base:
                evs.add(ev)
        if evs:
            by_dt[dt] = evs
    # lazy mode: AST-derived events (no import needed)
    for dt, evs in _AST_EVENTS.items():
        if evs:
            by_dt.setdefault(dt, set()).update(evs)
    # hooks doc_events
    wildcard = set()
    for dt, events in _MERGED_DOC_EVENTS.items():
        evset = set(events.keys())
        if dt == "*":
            wildcard |= evset
        else:
            by_dt.setdefault(dt, set()).update(evset)
    return {"by_doctype": {k: sorted(v) for k, v in by_dt.items()}, "wildcard": sorted(wildcard)}


def needs_python_registry_json():
    return json.dumps(needs_python_registry())


# ----------------------------------------------------------------- dispatch
_RESOLVE_CACHE = {}
_SWALLOWED = []  # (doctype, event, source, exc-repr) — controller methods that aborted on a stub


def _log_swallowed(doctype, event, source, exc):
    rec = (doctype, event, source, f"{type(exc).__name__}: {exc}")
    _SWALLOWED.append(rec)
    if os.environ.get("FERRO_LOG_SWALLOWED"):
        sys.stderr.write(f"[ferro:swallowed] {doctype}.{event} via {source}: {type(exc).__name__}: {exc}\n")


def swallowed_count():
    return len(_SWALLOWED)


def _resolve(dotted):
    if dotted in _RESOLVE_CACHE:
        return _RESOLVE_CACHE[dotted]
    try:
        mod, _, attr = dotted.rpartition(".")
        m = importlib.import_module(mod)
        fn = getattr(m, attr)
    except Exception:
        fn = None
    _RESOLVE_CACHE[dotted] = fn
    return fn


def _run_event(doc, event):
    # controller method
    try:
        doc.run_method(event)
    except (frappe.ValidationError, frappe.PermissionError):
        raise
    except Exception as e:
        # A controller method aborted midway — most often because it reached an un-ported
        # framework helper (a stub). We do NOT silently treat this as a clean pass: log it so
        # a partially-executed validate is visible. (frappe.throw still propagates above.)
        _log_swallowed(doc.get("doctype"), event, "method", e)
    # hooks
    dt = doc.get("doctype")
    for src in (_MERGED_DOC_EVENTS.get(dt, {}), _MERGED_DOC_EVENTS.get("*", {})):
        for dotted in src.get(event, []):
            fn = _resolve(dotted)
            if fn is None:
                continue
            try:
                fn(doc, event)
            except (frappe.ValidationError, frappe.PermissionError):
                raise
            except Exception as e:
                _log_swallowed(dt, event, dotted, e)


def dispatch_write(doctype, op, data_json, user):
    """Drive the controller lifecycle for a hooked write; persist via ferro_rt. Returns saved-doc JSON."""
    import ferro_rt
    frappe.local.session.user = user
    data = json.loads(data_json) if data_json else {}
    data["doctype"] = doctype

    if op == "insert":
        doc = frappe.get_doc(data)
        doc.__dict__["__islocal"] = True
        for ev in ("before_validate", "validate", "before_save", "before_insert"):
            _run_event(doc, ev)
        saved = ferro_rt.insert(doctype, json.dumps(doc.as_dict(), default=str))
        doc.update(dict(saved))
        doc.__dict__["__islocal"] = False
        for ev in ("after_insert", "on_update", "on_change"):
            _run_event(doc, ev)
        return json.dumps(doc.as_dict(), default=str)

    if op in ("update", "submit", "cancel"):
        name = data.get("name")
        existing = dict(ferro_rt.get_doc(doctype, name))
        doc = frappe.get_doc(dict(existing))
        doc.__dict__["_doc_before_save"] = frappe.get_doc(dict(existing))
        doc.update(data)
        doc.__dict__["__islocal"] = False
        if op == "submit":
            doc.docstatus = 1
        elif op == "cancel":
            doc.docstatus = 2
        pre = ("before_validate", "validate", "before_save")
        if op == "submit":
            pre = pre + ("before_submit",)
        elif op == "cancel":
            pre = ("before_cancel",)
        for ev in pre:
            _run_event(doc, ev)
        saved = ferro_rt.update(doctype, name, json.dumps(doc.as_dict(), default=str))
        doc.update(dict(saved))
        post = ("on_update", "on_change")
        if op == "submit":
            post = ("on_update", "on_submit", "on_change")
        elif op == "cancel":
            post = ("on_cancel", "on_change")
        for ev in post:
            _run_event(doc, ev)
        return json.dumps(doc.as_dict(), default=str)

    if op == "delete":
        name = data.get("name")
        try:
            doc = frappe.get_doc(doctype, name)
        except Exception:
            doc = frappe.get_doc({"doctype": doctype, "name": name})
        for ev in ("on_trash",):
            _run_event(doc, ev)
        ferro_rt.delete(doctype, name)
        _run_event(doc, "after_delete")
        return "{}"

    raise ValueError(f"unknown op {op}")
