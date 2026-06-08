#!/usr/bin/env python3
"""
Measure the RSS FLOOR of running Frappe app controller code on a THIN frappe shim,
WITHOUT the real framework (no werkzeug/redis/jinja/pymysql/requests/...).

Strategy:
  - Provide a permissive stub for `frappe`, the app packages, and heavy third-party libs,
    via a meta_path finder, so controller MODULE BODIES compile+exec (class defs, decorators,
    module-level constants) but no heavy dependency is actually loaded.
  - Load EVERY doctype controller (*/doctype/*/<name>.py) across all 5 apps (worst case:
    every controller resident at once).
  - Report VmRSS at: bare, after frappe stub, after loading ALL controllers; plus module count
    and the marshalled code size of the loaded controllers.
"""
import sys, os, types, gc, marshal, importlib.abc, importlib.machinery

# Bring-your-own app clones: set $FERRO_REPOS, or clone the 5 apps into ./repos (gitignored).
_HERE = os.path.dirname(os.path.abspath(__file__))
REPOS = os.environ.get("FERRO_REPOS", os.path.join(_HERE, "repos"))
APPS = ["crm", "helpdesk", "gameplan", "hrms", "erpnext"]

def rss_mb():
    for line in open("/proc/self/status"):
        if line.startswith("VmRSS:"):
            return int(line.split()[1]) / 1024.0
    return -1.0

def hwm_mb():
    for line in open("/proc/self/status"):
        if line.startswith("VmHWM:"):
            return int(line.split()[1]) / 1024.0
    return -1.0

# ---- Universal permissive stub object: works as base class, callable, attr/item access ----
class _Inst:
    def __init__(self, *a, **k): pass
    def __getattr__(self, k): return Universal
    def __call__(self, *a, **k): return Universal
    def __getitem__(self, k): return Universal
    def __setitem__(self, k, v): pass
    def __iter__(self): return iter(())
    def __enter__(self): return self
    def __exit__(self, *a): return False

class _Meta(type):
    def __getattr__(cls, k): return Universal
    def __call__(cls, *a, **k): return _Inst()
    def __getitem__(cls, k): return Universal

class Universal(metaclass=_Meta):
    pass

class _StubModule(types.ModuleType):
    def __getattr__(self, k):
        # whitelist decorator must behave when used as @frappe.whitelist or @frappe.whitelist(...)
        return Universal

# Heavy / third-party top-level names we refuse to really import (keep them lazy/out).
THIRD_PARTY = {
    "pandas","numpy","scipy","matplotlib","openpyxl","xlsxwriter","PIL","Pillow","lxml",
    "requests","pdfkit","weasyprint","reportlab","boto3","google","redis","pymysql",
    "werkzeug","jinja2","num2words","whoosh","oauthlib","pypdf","cssutils","bs4","rq",
    "croniter","pydantic","cryptography","babel","html5lib","chardet","charset_normalizer",
    "holidays","unidecode","markdown","premailer","passlib","pyotp","ldap3","zeep",
    "stripe","razorpay","braintree","plaid","gocardless_pro","tweepy","facebook","slugify",
    "phonenumbers","pycountry","geopy","sendgrid","twilio","posthog","sentry_sdk",
}
STUB_PREFIXES = set(APPS) | {"frappe"} | THIRD_PARTY

class _Finder(importlib.abc.MetaPathFinder, importlib.abc.Loader):
    def find_spec(self, name, path, target=None):
        top = name.split(".")[0]
        if top in STUB_PREFIXES:
            return importlib.machinery.ModuleSpec(name, self)
        return None
    def create_module(self, spec):
        return _StubModule(spec.name)
    def exec_module(self, module):
        pass

def inner_pkg_dir(app):
    """Frappe apps nest the python package one level down: <app>/<app>/."""
    cand = os.path.join(REPOS, app, app)
    return cand if os.path.isdir(cand) else os.path.join(REPOS, app)

def modname_for(app, path):
    base = inner_pkg_dir(app)
    rel = os.path.relpath(path, base)            # accounts/doctype/sales_invoice/sales_invoice.py
    parts = rel[:-3].split(os.sep)               # drop .py
    return app + "." + ".".join(parts)

def controller_files(scope="all"):
    full = scope.startswith("full:")
    sc = scope.split(":", 1)[1] if full else scope
    apps = APPS if sc in ("all", "hot") else [sc]
    out = []
    for app in apps:
        base = inner_pkg_dir(app)
        for root, _dirs, files in os.walk(base):
            rp = root.replace(os.sep, "/") + "/"
            if full:
                # every .py except tests/node_modules — the true resident ceiling
                if "/tests/" in rp or "/node_modules/" in rp:
                    continue
                for fn in files:
                    if fn.endswith(".py") and not fn.startswith("test_"):
                        out.append((app, os.path.join(root, fn)))
            else:
                if "/doctype/" not in rp:
                    continue
                leaf = os.path.basename(root)
                f = os.path.join(root, leaf + ".py")
                if os.path.isfile(f):
                    out.append((app, f))
    return out

def main():
    scope = sys.argv[1] if len(sys.argv) > 1 else "all"
    base_rss = rss_mb()
    print(f"[0] bare interpreter            RSS={base_rss:6.1f} MB  modules={len(sys.modules)}")

    sys.meta_path.insert(0, _Finder())
    import frappe  # noqa  (the stub)
    stub_rss = rss_mb()
    print(f"[1] + thin frappe stub          RSS={stub_rss:6.1f} MB  modules={len(sys.modules)}  (Δ {stub_rss-base_rss:+.1f})")

    files = controller_files(scope)
    if scope == "hot":
        # realistic working set: a sample of commonly-touched erpnext controllers
        hot = ("sales_invoice","sales_order","payment_entry","stock_entry","item","customer",
               "purchase_invoice","purchase_order","delivery_note","journal_entry","gl_entry",
               "account","supplier","quotation","material_request")
        files = [t for t in files if os.path.basename(t[1])[:-3] in hot][:50]
    print(f"--- scope={scope} ({len(files)} controllers) ---")
    ok = fail = 0
    marshal_bytes = 0
    errors = {}
    for app, path in files:
        name = modname_for(app, path)
        try:
            src = open(path, "r", encoding="utf-8").read()
            code = compile(src, path, "exec")
            marshal_bytes += len(marshal.dumps(code))
            mod = types.ModuleType(name)
            mod.__file__ = path
            mod.__name__ = name
            pkg = name.rsplit(".", 1)[0]
            mod.__package__ = pkg
            sys.modules[name] = mod
            exec(code, mod.__dict__)
            ok += 1
        except Exception as e:
            fail += 1
            k = type(e).__name__
            errors[k] = errors.get(k, 0) + 1
            sys.modules.pop(name, None)

    gc.collect()
    loaded_rss = rss_mb()
    print(f"[2] + ALL {ok} controllers exec'd  RSS={loaded_rss:6.1f} MB  modules={len(sys.modules)}  (Δ from stub {loaded_rss-stub_rss:+.1f})")
    print()
    print(f"controllers found     : {len(files)}")
    print(f"  exec OK             : {ok}")
    print(f"  exec FAILED         : {fail}   {errors}")
    print(f"marshalled code bytes : {marshal_bytes/1024/1024:6.2f} MB  (compiled code objects of OK+failed sources)")
    print()
    print(f"=== FLOOR: interp+stub+ALL app controllers resident = {loaded_rss:.1f} MB RSS (HWM {hwm_mb():.1f}) ===")
    print(f"=== vs real 'import frappe'+app worker baseline = 118 MB (framework cliff) ===")

if __name__ == "__main__":
    main()
