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
import os, sys, json, ast, types, importlib, importlib.abc, importlib.machinery, traceback

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
    # Resolve override_doctype_class hooks into the controller registry so get_controller() returns
    # the override (e.g. CRM maps Contact -> crm.overrides.contact.CustomContact, which adds the
    # default_list_data() that crm.api.doc.get_data calls — without this it falls back to the base
    # Document and 500s on Contact list views). Overrides win over the auto-registered base class.
    if mode == "all":
        for doctype, dotted in list(_OVERRIDE_CLASS.items()):
            try:
                modname, _, clsname = dotted.rpartition(".")
                module = importlib.import_module(modname)
                cls = getattr(module, clsname, None)
                if cls is not None:
                    frappe._register_controller(doctype, cls)
                    n_ctrl += 1
            except Exception as e:
                n_err += 1
                if verbose:
                    sys.stderr.write(f"[override-import-error] {dotted}: {e}\n")

    # Map of whitelisted methods across the loaded apps, built once on worker start so ferro can
    # auto-route any /api/method/<app>.* call it doesn't serve natively into Python (see call_method).
    _WHITELISTED.clear()
    for app in apps:
        _WHITELISTED.update(_scan_whitelisted(app))
    _LOADED.update(controllers=n_ctrl, doctypes=n_dt, import_errors=n_err, whitelisted=len(_WHITELISTED))
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


def resolve_doctype_method(doctype, method):
    """Mirror Frappe's v2 `/api/method/<doctype>/<method>` resolution: a doctype-scoped method runs
    on the doctype's controller MODULE (e.g. `GP Project`+`get_joined_spaces` ->
    `gameplan.gameplan.doctype.gp_project.gp_project.get_joined_spaces`). Returns the dotted path, or
    None if the doctype is unknown."""
    mod = _DOCTYPE_MODULE.get(doctype)
    if not mod:
        # not yet in the map (lazy mode): import-and-register the controller, which fills it.
        try:
            lazy_import_controller(doctype)
        except Exception:
            pass
        mod = _DOCTYPE_MODULE.get(doctype)
    return f"{mod}.{method}" if mod else None


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


# ----------------------------------------------------------------- whitelisted-method map
# Built on worker start (load()) so ferro can decide, per /api/method/<dotted> request, whether it
# is a real app endpoint to route into Python — without importing every api module up front.
_WHITELISTED = set()


def _is_whitelist_decorator(dec):
    """@frappe.whitelist, @frappe.whitelist(...), @whitelist, @whitelist(...)."""
    target = dec.func if isinstance(dec, ast.Call) else dec
    if isinstance(target, ast.Attribute):
        return target.attr == "whitelist"
    if isinstance(target, ast.Name):
        return target.id == "whitelist"
    return False


def _scan_whitelisted(app):
    """Static AST scan: dotted paths of every MODULE-LEVEL function decorated @frappe.whitelist in
    `app`. (Class-method whitelists are reached through frappe.client/run_doc_method, not by dotted
    path, so they're out of scope here.) No import — cheap and complete."""
    import glob
    base = _inner_pkg(app)
    out = set()
    for py in glob.glob(os.path.join(base, "**", "*.py"), recursive=True):
        if os.path.basename(py).startswith("test_"):
            continue
        try:
            tree = ast.parse(open(py, "r", encoding="utf-8").read())
        except Exception:
            continue
        mod = _modname(app, py)
        # a package's __init__.py is addressed by the package path, not `<pkg>.__init__`
        if mod.endswith(".__init__"):
            mod = mod[: -len(".__init__")]
        for node in tree.body:
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and any(
                _is_whitelist_decorator(d) for d in node.decorator_list
            ):
                out.add(f"{mod}.{node.name}")
    return out


def run_after_install(apps_json):
    """Run each app's `after_install` hook (crm.install.after_install, …) so the app's master/seed
    data exists — default statuses, fields layouts, industries, etc. ferro's native install-app
    materialises DocType *schema* only, so without this the app's frontends crash on the missing
    seed data (e.g. CRM's status badges read .name of an absent CRM Lead Status). Runs under the
    embedded interpreter with the request connection set, so frappe.get_doc(...).insert() writes to
    the real SQLite site. Returns a JSON summary."""
    apps = json.loads(apps_json) if apps_json else []
    done, errors = [], []
    for app in apps:
        try:
            mod = importlib.import_module(f"{app}.hooks")
        except Exception as e:
            errors.append(f"{app}.hooks: {e}")
            continue
        hook = getattr(mod, "after_install", None)
        if not hook:
            continue
        for dotted in ([hook] if isinstance(hook, str) else list(hook)):
            fn = _resolve(dotted)
            if fn is None:
                errors.append(f"{dotted}: not resolvable")
                continue
            try:
                fn()
                done.append(dotted)
            except Exception as e:
                if os.environ.get("FERRO_TRACE"):
                    traceback.print_exc()
                errors.append(f"{dotted}: {type(e).__name__}: {e}")
    return json.dumps({"ran": done, "errors": errors})


def whitelisted_methods_json():
    """The map ferro pulls at boot to gate auto-routing. Sorted list of dotted paths."""
    return json.dumps(sorted(_WHITELISTED))


def is_whitelisted(method):
    return method in _WHITELISTED


def call_method(method, args_json, user):
    """Invoke a whitelisted method by dotted path with the request's form args, returning a
    {"message": <retval>} JSON envelope (frappe.handler.execute_cmd semantics, minimally). ferro
    only calls this for methods already in the whitelisted map. Raises on a missing/uncallable
    target or a controller error; ferrod maps the exception to an HTTP status."""
    import inspect

    frappe.local.session.user = user
    args = json.loads(args_json) if args_json else {}
    if not isinstance(args, dict):
        args = {}
    # populate frappe.form_dict (and the module alias) the way the real handler does
    frappe.local.form_dict = frappe._dict(args)
    frappe.form_dict = frappe.local.form_dict

    fn = _resolve(method)
    if fn is None or not callable(fn):
        raise LookupError(f"whitelisted method not found: {method}")

    # Pass only the args the function actually accepts (unless it takes **kwargs) — Frappe filters
    # the form_dict to the signature so stray query params (cmd, _, csrf…) don't blow up the call.
    try:
        sig = inspect.signature(fn)
        if any(p.kind == p.VAR_KEYWORD for p in sig.parameters.values()):
            kwargs = dict(args)
        else:
            accepted = set(sig.parameters)
            kwargs = {k: v for k, v in args.items() if k in accepted}
    except (TypeError, ValueError):
        kwargs = {}

    try:
        result = fn(**kwargs)
    except Exception:
        if os.environ.get("FERRO_TRACE"):
            traceback.print_exc()
        raise
    return json.dumps({"message": result}, default=str)


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
