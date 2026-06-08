"""
frappe — a thin, native-backed shim that stands in for the Frappe framework.

Frappe's ~106 MB import cliff is the *framework's* own machinery (werkzeug/jinja/redis/
pymysql/requests/...). ferro reimplements that data plane in Rust, so the apps don't need it:
they import THIS package, whose hot symbols delegate into the native `ferro_rt` module
(ferro's ORM/meta/SQL), with a permissive lazy fallback for the long tail of framework
internals an individual controller might touch but the request path doesn't exercise.

The goal: every real Frappe app controller IMPORTS for real (so its code is resident and the
memory ceiling is honest), and the hot CRUD/lifecycle path EXECUTES for real.
"""
import json as _json
import sys as _sys

try:
    import ferro_rt as _rt
except ImportError:  # allow `import frappe` under plain CPython (measurement without the native module)
    _rt = None

# --------------------------------------------------------------------------- _dict
class _dict(dict):
    """frappe._dict — attribute-accessible dict (used everywhere in Frappe code)."""
    def __getattr__(self, k):
        try:
            return self[k]
        except KeyError:
            return None
    def __setattr__(self, k, v):
        self[k] = v
    def __delattr__(self, k):
        try:
            del self[k]
        except KeyError:
            raise AttributeError(k)
    def copy(self):
        return _dict(self)


# --------------------------------------------------------------------------- exceptions
from frappe.exceptions import (  # noqa: E402
    ValidationError, PermissionError, DoesNotExistError, DuplicateEntryError,
    MandatoryError, FrappeException, AuthenticationError, LinkExistsError,
)

# --------------------------------------------------------------------------- request context
local = _dict()
local.flags = _dict()
local.session = _dict(user="Administrator", sid="ferro")
local.form_dict = _dict()
local.lang = "en"
local.site = "ferro"
local.response = _dict()
local.message_log = []

flags = local.flags
session = local.session
form_dict = local.form_dict
request = None
conf = _dict()
lang = "en"


def _set_user(user):
    local.session.user = user
    if _rt is not None:
        pass  # current user is tracked Rust-side via set_user()


# --------------------------------------------------------------------------- translation
def _(msg, lang=None, context=None):
    return msg

def bold(text):
    return "<b>{0}</b>".format(text)


# --------------------------------------------------------------------------- messages / errors
def msgprint(msg, *a, **k):
    m = _as_text(msg)
    local.message_log.append(m)
    if _rt is not None:
        _rt.msgprint(m)

def throw(msg, exc=ValidationError, title=None, **k):
    raise exc(_as_text(msg))

def log_error(title=None, message=None, reference_doctype=None, reference_name=None, *a, **k):
    return None

def _as_text(msg):
    if isinstance(msg, (list, dict)):
        return _json.dumps(msg, default=str)
    return str(msg)


# --------------------------------------------------------------------------- db facade
from frappe.database import Database as _Database  # noqa: E402
db = _Database(_rt)

# qb (query builder)
from frappe import query_builder as _qb_mod  # noqa: E402
qb = _qb_mod


# --------------------------------------------------------------------------- ORM front-door
from frappe.model.document import Document, get_controller  # noqa: E402


def get_doc(*args, **kwargs):
    """frappe.get_doc — either load an existing doc, or build a new (unsaved) one from a dict."""
    if args and isinstance(args[0], dict):
        data = dict(args[0])
        data.update(kwargs)
        dt = data.get("doctype")
        return _new_controller(dt, data)
    if kwargs and "doctype" in kwargs and len(args) <= 1 and not (args and isinstance(args[0], str) and len(args) > 1):
        data = dict(kwargs)
        return _new_controller(data.get("doctype"), data)
    # get_doc("DocType", "name")
    doctype = args[0]
    name = args[1] if len(args) > 1 else kwargs.get("name")
    if name is None:
        # single
        data = _rt.get_doc(doctype, doctype) if _rt else {}
        return _new_controller(doctype, dict(data))
    data = _rt.get_doc(doctype, name)
    return _new_controller(doctype, dict(data))


def new_doc(doctype, **kwargs):
    data = {"doctype": doctype}
    data.update(kwargs)
    return _new_controller(doctype, data)


def get_cached_doc(*args, **kwargs):
    return get_doc(*args, **kwargs)


def get_last_doc(doctype, filters=None, order_by="creation desc"):
    rows = get_all(doctype, filters=filters, order_by=order_by, limit_page_length=1)
    if not rows:
        raise DoesNotExistError(doctype)
    return get_doc(doctype, rows[0]["name"])


def get_all(doctype, filters=None, fields=None, order_by=None, limit_start=0,
            limit_page_length=0, limit=None, pluck=None, **kwargs):
    if limit is not None:
        limit_page_length = limit
    fj = _json.dumps(fields) if fields else None
    flj = _json.dumps(filters) if filters else None
    rows = _rt.get_list(doctype, flj, fj, order_by, int(limit_start or 0), int(limit_page_length or 0))
    rows = [_dict(r) for r in rows]
    if pluck:
        return [r.get(pluck) for r in rows]
    return rows


def get_list(doctype, **kwargs):
    return get_all(doctype, **kwargs)


def get_value(doctype, filters=None, fieldname="name", **kwargs):
    return db.get_value(doctype, filters, fieldname, **kwargs)


def get_cached_value(doctype, name, fieldname="name", **kwargs):
    return db.get_value(doctype, name, fieldname, **kwargs)


def get_single_value(doctype, fieldname):
    return db.get_single_value(doctype, fieldname)


def get_single(doctype):
    return get_doc(doctype)


def get_doc_before_save(doc):
    return getattr(doc, "_doc_before_save", None)


def delete_doc(doctype, name=None, *a, **k):
    if _rt is not None:
        _rt.delete(doctype, name)


def rename_doc(doctype, old, new, *a, **k):
    return new


def db_exists(doctype, name=None):
    return db.exists(doctype, name)
exists = db_exists


def set_value(doctype, name, fieldname, value=None):
    return db.set_value(doctype, name, fieldname, value)


# --------------------------------------------------------------------------- meta / permissions
def get_meta(doctype, cached=True):
    from frappe.model.meta import Meta as _Meta
    return _Meta(doctype)


def get_roles(user=None):
    return ["System Manager", "All"]


def has_permission(doctype, ptype="read", doc=None, user=None, *a, **k):
    return True


def get_hooks(hook=None, default=None, app_name=None):
    from frappe.boot import get_hooks as _gh
    return _gh(hook, default, app_name)


def whitelist(allow_guest=False, methods=None, xss_safe=False, **kwargs):
    """@frappe.whitelist — mark a function callable via /api/method. Returns the function."""
    def _decorator(fn):
        fn._is_whitelisted = True
        fn._allow_guest = allow_guest
        return fn
    if callable(allow_guest):  # used as bare @frappe.whitelist
        fn = allow_guest
        fn._is_whitelisted = True
        return fn
    return _decorator


# --------------------------------------------------------------------------- helpers used widely
def scrub(txt):
    return (txt or "").replace(" ", "_").replace("-", "_").lower()


def unscrub(txt):
    return (txt or "").replace("_", " ").replace("-", " ").title()


def generate_hash(txt=None, length=None):
    h = _rt.generate_hash() if _rt else "0123456789abcdef0123456789abcdef"
    if length:
        return h[:length]
    return h


def parse_json(val):
    if isinstance(val, str):
        try:
            return _json.loads(val)
        except Exception:
            return val
    return val


def as_json(obj, indent=None):
    return _json.dumps(obj, default=str, indent=indent)


def cstr(s, encoding="utf-8"):
    from frappe.utils import cstr as _c
    return _c(s)


# --------------------------------------------------------------------------- background / realtime (out-of-budget: no-op here)
def enqueue(method, *a, **k):
    return None

def enqueue_doc(*a, **k):
    return None

def publish_realtime(*a, **k):
    return None

def publish_progress(*a, **k):
    return None

def sendmail(*a, **k):
    return None

def get_system_settings(key):
    return None

def cache():
    return _NoCache()

class _NoCache:
    def get_value(self, *a, **k): return None
    def set_value(self, *a, **k): return None
    def hget(self, *a, **k): return None
    def hset(self, *a, **k): return None
    def delete_value(self, *a, **k): return None
    def delete_key(self, *a, **k): return None


# --------------------------------------------------------------------------- controller resolution
_CONTROLLER_REGISTRY = {}   # doctype -> python class (filled by ferro_boot)
_DEFAULT_DEFAULTS = {}
_lazy_import_controller = None   # set by ferro_boot in lazy mode


def _register_controller(doctype, cls):
    _CONTROLLER_REGISTRY[doctype] = cls


def _new_controller(doctype, data):
    cls = _CONTROLLER_REGISTRY.get(doctype)
    if cls is None and _lazy_import_controller is not None and doctype:
        cls = _lazy_import_controller(doctype)   # import on first touch
    if cls is None:
        cls = Document
    doc = cls.__new__(cls)
    Document.__init__(doc, data)
    return doc


# convenience aliases Frappe exposes at top level
def get_traceback(*a, **k):
    return ""

def get_request_header(*a, **k):
    return None

def get_url(*a, **k):
    return ""

def has_common(a, b):
    return bool(set(a or []) & set(b or []))

def get_meta_module(*a, **k):
    return None


# A generic passthrough decorator name a few modules apply at import time.
def validate_and_sanitize_search_inputs(fn):
    return fn


def read_only():
    def _d(fn):
        return fn
    return _d


def real_time(*a, **k):
    def _d(fn):
        return fn
    return _d


def __getattr__(name):
    # permissive fallback for the long tail of top-level frappe.* names a controller may import
    from frappe._lazy import stub_attr
    return stub_attr(name)


__version__ = "ferro-shim-0"
