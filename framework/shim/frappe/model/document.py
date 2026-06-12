"""
frappe.model.document — the Document base class app controllers subclass.

Backed by ferro_rt for persistence. This is the real lifecycle contract (validate / before_save
/ on_update / insert / save / submit / db_set / as_dict / get / set / append / meta), trimmed to
what app controllers actually call, so a real controller's methods run unmodified on ferro.
"""
import json as _json

try:
    import ferro_rt as _rt
except ImportError:
    _rt = None

from frappe.model.meta import Meta
from frappe.exceptions import NameError as _DuplicateName  # frappe's, raised on a name conflict


def _native_insert(doctype, data, raw=False, ignore_mandatory=False, ignore_links=False):
    """Insert via ferro_rt, mapping the native "already exists" ValueError to frappe.NameError —
    real Frappe raises NameError on a duplicate name, and idempotent installers catch exactly that.
    `raw=True` -> db_insert semantics (no validation at all). `ignore_mandatory`/`ignore_links`
    mirror the matching `doc.insert(...)` flags (ERPNext's COA importer needs ignore_mandatory)."""
    if _rt is None:
        return data
    payload = _json.dumps(data, default=str)
    try:
        if raw:
            fn = getattr(_rt, "raw_insert", None)
            return fn(doctype, payload) if fn else _rt.insert(doctype, payload, True, True)
        return _rt.insert(doctype, payload, ignore_mandatory, ignore_links)
    except ValueError as e:
        if "already exists" in str(e):
            raise _DuplicateName(str(e))
        raise
    except TypeError:
        # Older native module without the flag params — fall back to the 2-arg call.
        return _rt.insert(doctype, payload)


class BaseDocument:
    def __init__(self, data=None):
        self.__dict__["_table_fieldnames"] = set()
        if isinstance(data, dict):
            self.update(data)

    # ---- attribute / field access ----
    def __getattr__(self, key):
        # Only called when normal lookup fails: treat unknown non-dunder names as unset fields.
        if key.startswith("__") and key.endswith("__"):
            raise AttributeError(key)
        return None

    # `doc.flags` is a mutable namespace controllers freely mutate (doc.flags.ignore_mandatory =
    # True). A property (data descriptor) guarantees it's always a frappe._dict, even if a field
    # named "flags" got written into __dict__ by a set()/update() — attribute assignment then works.
    @property
    def flags(self):
        f = self.__dict__.get("_flags")
        if not isinstance(f, dict):
            from frappe import _dict
            f = _dict()
            self.__dict__["_flags"] = f
        return f

    @flags.setter
    def flags(self, value):
        from frappe import _dict
        self.__dict__["_flags"] = value if isinstance(value, dict) else _dict()

    def get(self, key, default=None, filters=None):
        if isinstance(key, dict):
            # get(filters) over a child table is uncommon; return matching children
            return self._get_children_filtered(key)
        val = self.__dict__.get(key, default)
        return val if val is not None else default

    def set(self, key, value, as_value=False):
        self.__dict__[key] = value
        return value

    def update(self, d):
        if not d:
            return self
        for k, v in dict(d).items():
            self.__dict__[k] = v
        return self

    def __setitem__(self, k, v):
        self.__dict__[k] = v

    def __getitem__(self, k):
        return self.__dict__.get(k)

    def setdefault(self, key, default):
        if self.__dict__.get(key) is None:
            self.__dict__[key] = default
        return self.__dict__[key]

    def get_value(self, fieldname):
        return self.get(fieldname)

    def set_value(self, fieldname, value):
        return self.set(fieldname, value)

    # ---- child tables ----
    def _get_children_filtered(self, filters):
        return []

    def get_all_children(self, parenttype=None):
        out = []
        for fn in self.__dict__.get("_table_fieldnames", ()):
            rows = self.__dict__.get(fn) or []
            if isinstance(rows, list):
                out.extend(rows)
        return out

    def append(self, key, value=None):
        if value is None:
            value = {}
        rows = self.__dict__.get(key)
        if not isinstance(rows, list):
            rows = []
            self.__dict__[key] = rows
        self.__dict__["_table_fieldnames"].add(key)
        if isinstance(value, BaseDocument):
            child = value
        else:
            child = Document(dict(value, doctype=value.get("doctype") if isinstance(value, dict) else None))
        child.parent = self.get("name")
        child.parentfield = key
        child.parenttype = self.get("doctype")
        child.idx = len(rows) + 1
        rows.append(child)
        return child

    def extend(self, key, rows):
        for r in rows or []:
            self.append(key, r)

    def remove(self, child):
        for fn in list(self.__dict__.get("_table_fieldnames", ())):
            rows = self.__dict__.get(fn) or []
            if child in rows:
                rows.remove(child)

    # ---- serialization ----
    def as_dict(self, no_nulls=False, convert_dates_to_str=False, no_default_fields=False, **k):
        out = {}
        for key, val in self.__dict__.items():
            if key.startswith("_") and key not in (
                "_user_tags", "_comments", "_assign", "_liked_by"
            ):
                continue
            if isinstance(val, list):
                out[key] = [r.as_dict() if isinstance(r, BaseDocument) else r for r in val]
            elif isinstance(val, BaseDocument):
                out[key] = val.as_dict()
            else:
                if no_nulls and val is None:
                    continue
                out[key] = val
        return out

    def get_valid_dict(self, *a, **k):
        # parent-table columns only (exclude child tables) for the ORM write
        out = {}
        tables = self.__dict__.get("_table_fieldnames", set())
        for key, val in self.__dict__.items():
            if key.startswith("_") and key not in ("_user_tags", "_comments", "_assign", "_liked_by"):
                continue
            if key in tables or isinstance(val, (BaseDocument,)):
                continue
            if isinstance(val, list):
                continue
            out[key] = val
        return out

    def as_json(self):
        return _json.dumps(self.as_dict(), default=str)

    # ---- meta ----
    @property
    def meta(self):
        m = self.__dict__.get("_meta")
        if m is None:
            m = Meta(self.get("doctype"))
            self.__dict__["_meta"] = m
        return m

    def get_field(self, fieldname):
        return self.meta.get_field(fieldname)

    def get_precision(self, fieldname, parentfield=None):
        return 2

    def precision(self, fieldname, parentfield=None):
        return 2


class Document(BaseDocument):
    def __init__(self, *args, **kwargs):
        super().__init__()
        self.__dict__["flags"] = __import__("frappe")._dict()
        if args and isinstance(args[0], dict):
            data = dict(args[0])
            data.update(kwargs)
            self.update(data)
            self._wire_children()
        elif args and isinstance(args[0], str):
            self.doctype = args[0]
            if len(args) > 1 and args[1] is not None:
                loaded = _rt.get_doc(args[0], args[1]) if _rt else {}
                self.update(dict(loaded))
                self._wire_children()
        if "__islocal" not in self.__dict__:
            self.__dict__["__islocal"] = not bool(self.get("name"))

    def _wire_children(self):
        # turn list-of-dict child rows into Document proxies
        for key, val in list(self.__dict__.items()):
            if isinstance(val, list) and val and all(isinstance(r, dict) for r in val):
                self.__dict__["_table_fieldnames"].add(key)
                self.__dict__[key] = [Document(r) for r in val]

    # ---- state queries ----
    def is_new(self):
        return bool(self.__dict__.get("__islocal", True))

    def get_doc_before_save(self):
        return self.__dict__.get("_doc_before_save")

    def get_value_before_save(self, fieldname):
        prev = self.get_doc_before_save()
        return prev.get(fieldname) if prev else None

    def has_value_changed(self, fieldname):
        prev = self.get_doc_before_save()
        if prev is None:
            return True
        return prev.get(fieldname) != self.get(fieldname)

    def get_db_value(self, fieldname):
        if _rt is None or self.is_new():
            return None
        return _rt.get_value(self.get("doctype"), self.get("name"), fieldname)

    # ---- run a controller method (+ does NOT itself fire hooks; ferro driver does) ----
    def run_method(self, method, *args, **kwargs):
        fn = getattr(self, method, None)
        if callable(fn):
            return fn(*args, **kwargs)
        return None

    def _validate(self):
        return None

    # ---- persistence (delegates to ferro_rt) ----
    def insert(self, ignore_permissions=False, ignore_links=False, ignore_if_duplicate=False,
               ignore_mandatory=False, set_name=None, set_child_names=True, **k):
        # run the controller's own before-save methods (frappe does this inside insert)
        for m in ("before_validate", "validate", "before_save", "before_insert"):
            self.run_method(m)
        data = self.as_dict()
        # Honour both the explicit kwargs and the controller-set doc.flags (real Frappe ORs them).
        im = bool(ignore_mandatory or self.flags.get("ignore_mandatory"))
        il = bool(ignore_links or self.flags.get("ignore_links"))
        try:
            saved = _native_insert(self.get("doctype"), data, ignore_mandatory=im, ignore_links=il)
        except _DuplicateName:
            if ignore_if_duplicate:
                return self
            raise
        self.update(dict(saved))
        self.__dict__["__islocal"] = False
        for m in ("after_insert", "on_update", "on_change"):
            self.run_method(m)
        return self

    def db_insert(self, *a, **k):
        """Low-level insert (real Frappe: no validate/hooks). The shim routes it to the same native
        insert; close enough for the bootstrap inserts (UOM, defaults) that use it."""
        data = self.as_dict()
        saved = _native_insert(self.get("doctype"), data, raw=True)
        self.update(dict(saved))
        self.__dict__["__islocal"] = False
        return self

    def db_update(self, *a, **k):
        if self.is_new():
            return self.db_insert()
        data = self.as_dict()
        if _rt:
            _rt.update(self.get("doctype"), self.get("name"), _json.dumps(data, default=str))
        return self

    def save(self, ignore_permissions=False, ignore_version=False, **k):
        if self.is_new():
            return self.insert(**k)
        # Snapshot the pre-save DB version so controllers' get_doc_before_save() /
        # has_value_changed() / get_value_before_save() work during validate (real Frappe loads this
        # before running save hooks). Loaded as a controller so child tables stay attribute- and
        # .get()-accessible. Best-effort: a failure just leaves the snapshot absent (prior behaviour).
        if self.__dict__.get("_doc_before_save") is None and _rt is not None:
            try:
                import frappe as _f
                before = _f.get_doc(self.get("doctype"), self.get("name"))
                # Child tables aren't always materialised on a plain load; default them to [] so
                # controllers iterating get_doc_before_save().get("<table>") don't hit None.
                try:
                    for tf in _f.get_meta(self.get("doctype")).get_table_fields():
                        if before.get(tf.get("fieldname")) is None:
                            before.set(tf.get("fieldname"), [])
                except Exception:
                    pass
                self.__dict__["_doc_before_save"] = before
            except Exception:
                pass
        for m in ("before_validate", "validate", "before_save"):
            self.run_method(m)
        data = self.as_dict()
        saved = _rt.update(self.get("doctype"), self.get("name"), _json.dumps(data, default=str)) if _rt else data
        self.update(dict(saved))
        for m in ("on_update", "on_change"):
            self.run_method(m)
        return self

    def submit(self):
        self.docstatus = 1
        return self.save()

    def cancel(self):
        self.docstatus = 2
        return self.save()

    def delete(self, ignore_permissions=False, **k):
        if _rt is not None and self.get("name"):
            _rt.delete(self.get("doctype"), self.get("name"))

    def reload(self):
        if _rt is not None and self.get("name"):
            self.update(dict(_rt.get_doc(self.get("doctype"), self.get("name"))))
        return self

    def db_set(self, fieldname, value=None, update_modified=True, notify=False, commit=False, **k):
        if isinstance(fieldname, dict):
            for f, v in fieldname.items():
                self.set(f, v)
                if not self.is_new() and _rt is not None:
                    _rt.set_value(self.get("doctype"), self.get("name"), f, _json.dumps(v, default=str))
            return
        self.set(fieldname, value)
        if not self.is_new() and _rt is not None:
            _rt.set_value(self.get("doctype"), self.get("name"), fieldname, _json.dumps(value, default=str))

    def db_get(self, fieldname):
        return self.get_db_value(fieldname)

    def notify_update(self):
        pass

    def check_permission(self, *a, **k):
        pass

    def add_comment(self, *a, **k):
        return None

    def get_title(self):
        return self.get(self.meta.get("title_field") or "name")

    def set_onload(self, key, value):
        pass

    def get_onload(self, key=None):
        return None

    def queue_action(self, action, **kwargs):
        return getattr(self, action)()

    def add_tag(self, *a, **k):
        pass


def get_controller(doctype):
    import frappe
    return frappe._CONTROLLER_REGISTRY.get(doctype, Document)


def bulk_insert(*a, **k):
    return None


def __getattr__(name):
    from frappe._lazy import stub_attr
    return stub_attr(name)
