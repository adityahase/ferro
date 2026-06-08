#!/usr/bin/env python3
"""Block one or more top-level packages at import time, then import frappe +
frappe.app, init/connect, and serve 2 real API requests. Reports RSS, module
count, and whether the basic request path still works.

If it still serves 200s with X blocked, X is deferrable (delta RSS = the saving
from lazy-importing it). If import/serve breaks, X is eagerly required today and
needs a code change in Frappe to defer.

env BLOCK="pkg1,pkg2"  (comma sep top-level names). cwd=sites/, bench env python.
"""
import os
import sys

BLOCK = [b for b in os.environ.get("BLOCK", "").split(",") if b]


class Blocker:
    def find_spec(self, name, path=None, target=None):
        top = name.split(".")[0]
        if top in BLOCK:
            raise ModuleNotFoundError(f"blocked:{name}")
        return None


if BLOCK:
    sys.meta_path.insert(0, Blocker())


def rss_mb():
    for l in open("/proc/self/smaps_rollup"):
        if l.startswith("Rss:"):
            return int(l.split()[1]) / 1024
    return 0


def main():
    label = ",".join(BLOCK) or "BASELINE"
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
        A = {"X-Frappe-Site-Name": os.environ["MB_SITE"] if "MB_SITE" in os.environ else "mysite.sqlite",
             "Authorization": f"token {key}:{sec}"}
        codes.append(c.get("/api/method/frappe.ping", headers={"X-Frappe-Site-Name": A["X-Frappe-Site-Name"]}).status_code)
        codes.append(c.get("/api/method/frappe.auth.get_logged_user", headers=A).status_code)
        codes.append(c.get("/api/method/frappe.client.get_list?doctype=User&limit_page_length=5", headers=A).status_code)
        nmod = len(sys.modules)
    except Exception as e:
        status = f"BROKE:{type(e).__name__}:{str(e)[:60]}"
    import gc
    gc.collect()
    print(f"{label}\tRSS_MB={rss_mb():.2f}\tn_modules={nmod}\tcodes={codes}\t{status}")


if __name__ == "__main__":
    main()
