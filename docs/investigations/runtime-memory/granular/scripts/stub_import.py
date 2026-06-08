#!/usr/bin/env python3
"""Replace feature-only leaf libraries with lightweight stub modules BEFORE
importing frappe, to estimate the combined RSS/module saving achievable by
lazy-importing them. If frappe import + a basic API request still succeed, the
stubbed libs are genuinely not needed on the serve path and the delta is real.

env STUB="pkg1,pkg2,..."  cwd=sites/, bench env python.
"""
import os
import sys
import types

STUB = [s for s in os.environ.get("STUB", "").split(",") if s]


class _Dummy:
    def __call__(self, *a, **k):
        return _Dummy()

    def __getattr__(self, n):
        return _Dummy()

    def __mro_entries__(self, bases):
        return (object,)

    def __iter__(self):
        return iter(())


class StubModule(types.ModuleType):
    __path__ = []  # mark as package so submodule imports resolve to more stubs

    def __getattr__(self, n):
        if n.startswith("__") and n.endswith("__"):
            raise AttributeError(n)
        return _Dummy()


class StubFinder:
    def find_spec(self, name, path=None, target=None):
        top = name.split(".")[0]
        if top in STUB:
            import importlib.machinery
            return importlib.machinery.ModuleSpec(name, _StubLoader())
        return None


class _StubLoader:
    def create_module(self, spec):
        return StubModule(spec.name)

    def exec_module(self, module):
        pass


if STUB:
    sys.meta_path.insert(0, StubFinder())


def rss_mb():
    for l in open("/proc/self/smaps_rollup"):
        if l.startswith("Rss:"):
            return int(l.split()[1]) / 1024
    return 0


def main():
    label = ",".join(STUB) or "BASELINE"
    status = "ok"
    codes = []
    nmod = -1
    try:
        import frappe
        import frappe.app
        frappe.init(site=os.environ.get("MB_SITE", "mysite.sqlite"), sites_path=".")
        frappe.connect()
        frappe.set_user("Administrator")
        from frappe.core.doctype.user.user import generate_keys
        sec = generate_keys("Administrator")["api_secret"]
        frappe.db.commit()
        key = frappe.db.get_value("User", "Administrator", "api_key")
        from werkzeug.test import Client
        c = Client(frappe.app.application)
        site = os.environ.get("MB_SITE", "mysite.sqlite")
        A = {"X-Frappe-Site-Name": site, "Authorization": f"token {key}:{sec}"}
        codes.append(c.get("/api/method/frappe.ping", headers={"X-Frappe-Site-Name": site}).status_code)
        codes.append(c.get("/api/method/frappe.auth.get_logged_user", headers=A).status_code)
        codes.append(c.get("/api/method/frappe.client.get_list?doctype=User&limit_page_length=5", headers=A).status_code)
        nmod = len(sys.modules)
    except Exception as e:
        status = f"BROKE:{type(e).__name__}:{str(e)[:70]}"
    import gc
    gc.collect()
    print(f"RSS_MB={rss_mb():.2f}\tn_modules={nmod}\tcodes={codes}\t{status}\tSTUB={label}")


if __name__ == "__main__":
    main()
