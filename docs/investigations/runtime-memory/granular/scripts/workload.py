#!/usr/bin/env python3
"""Realistic Frappe *web-worker* workload (actual usage, not idle boot).

Exposes run_workload() so multiple measurement drivers (truth / tracemalloc /
memray / smaps) can warm the *same* code paths a production gunicorn worker
exercises while serving requests:

  - frappe.init + connect (SQLite) + set_user(Administrator)
  - meta/controller warmup across a broad doctype set
  - ORM reads: get_all / get_list / get_value / get_count / get_doc + as_dict
  - ORM writes: insert / save / delete inside transactions (commit + rollback)
  - Jinja templating + value formatting
  - REAL WSGI request handling through frappe.app.application via the werkzeug
    test client (routing, auth middleware, rate-limit, orjson response build),
    both guest and api-key-authenticated.

Run several `rounds` so lazy imports / caches reach steady state — i.e. the
heap of a worker that has *already served traffic*, which is what we measure.

Invocation contract: cwd must be the bench `sites/` dir; uses the bench env
python. SITE defaults to mysite.sqlite.
"""
import os
import sys

SITE = os.environ.get("MB_SITE", "mysite.sqlite")
ROUNDS = int(os.environ.get("MB_ROUNDS", "3"))

# Broad doctype set to warm meta + controllers (core, heavily used in a desk worker)
WARM_DOCTYPES = [
    "User", "DocType", "DocField", "DocPerm", "Role", "Report", "Custom Field",
    "Property Setter", "Workflow", "Workflow State", "Workflow Action",
    "Print Format", "File", "Email Queue", "Email Account", "Notification",
    "ToDo", "Tag", "Comment", "Activity Log", "Access Log", "View Log",
    "Communication", "Contact", "Address", "Dynamic Link", "Module Def",
    "Page", "Web Page", "Web Form", "Website Settings", "System Settings",
    "Singles", "Series", "DefaultValue", "Session Default", "Translation",
    "Language", "Country", "Currency", "Custom DocPerm", "Has Role",
    "Dashboard", "Dashboard Chart", "Number Card", "Letter Head",
]

# Read-friendly doctypes that surely have/allow rows or are queryable empty
READ_DOCTYPES = [
    "User", "DocType", "Role", "Module Def", "DocField", "Report",
    "Print Format", "Page", "Workflow State", "Language", "Country", "Currency",
    "ToDo", "Tag", "Comment", "File", "Email Queue", "Custom Field",
    "Property Setter", "Has Role",
]


def _warm_meta(frappe):
    n = 0
    for dt in WARM_DOCTYPES:
        try:
            m = frappe.get_meta(dt)
            _ = m.fields  # touch fields
            _ = m.get_valid_columns()
            n += 1
        except Exception:
            pass
    return n


def _orm_reads(frappe):
    n = 0
    for dt in READ_DOCTYPES:
        try:
            frappe.get_all(dt, fields=["name"], limit=20)
            frappe.db.count(dt)
            n += 1
        except Exception:
            pass
    # richer reads
    try:
        frappe.get_all("DocType", fields=["name", "module", "issingle", "istable"],
                       filters={"istable": 0}, order_by="modified desc", limit=50)
    except Exception:
        pass
    for name in ("Administrator", "Guest"):
        try:
            d = frappe.get_doc("User", name)
            d.as_dict()
            n += 1
        except Exception:
            pass
    try:
        ss = frappe.get_single("System Settings")
        ss.as_dict()
    except Exception:
        pass
    return n


def _orm_writes(frappe):
    n = 0
    made = []
    for i in range(10):
        try:
            d = frappe.get_doc({
                "doctype": "ToDo",
                "description": f"granular-mem workload row {i} " + ("x" * 40),
                "priority": "Medium",
            }).insert(ignore_permissions=True)
            made.append(d.name)
            n += 1
        except Exception:
            pass
    frappe.db.commit()
    # update
    for name in made[:5]:
        try:
            d = frappe.get_doc("ToDo", name)
            d.description = d.description + " [updated]"
            d.save(ignore_permissions=True)
        except Exception:
            pass
    frappe.db.commit()
    # delete all we made
    for name in made:
        try:
            frappe.delete_doc("ToDo", name, ignore_permissions=True, force=True)
        except Exception:
            pass
    frappe.db.commit()
    # a rollback path
    try:
        frappe.get_doc({"doctype": "ToDo", "description": "rolled-back"}).insert(ignore_permissions=True)
        frappe.db.rollback()
    except Exception:
        pass
    return n


def _templating(frappe):
    try:
        frappe.render_template(
            "Hello {{ user }} — {{ items|length }} items: "
            "{% for i in items %}{{ i }} {% endfor %}",
            {"user": frappe.session.user, "items": list(range(25))},
        )
    except Exception:
        pass
    try:
        from frappe.utils import formatdate, fmt_money, now_datetime
        formatdate(now_datetime())
        fmt_money(123456.789, currency="USD")
        frappe.format_value(123456.789, {"fieldtype": "Currency"})
    except Exception:
        pass


def _wsgi_requests(frappe, client, auth_headers, guest_headers):
    statuses = []
    reqs = [
        ("GET", "/api/method/frappe.ping", guest_headers, None),
        ("GET", "/api/method/frappe.auth.get_logged_user", auth_headers, None),
        ("GET", "/api/method/frappe.client.get_list?doctype=DocType&fields=[\"name\",\"module\"]&limit_page_length=20", auth_headers, None),
        ("GET", "/api/method/frappe.client.get_count?doctype=User", auth_headers, None),
        ("GET", "/api/resource/ToDo?limit_page_length=10", auth_headers, None),
        ("GET", "/api/resource/User?fields=[\"name\",\"email\"]&limit_page_length=5", auth_headers, None),
        ("GET", "/api/method/frappe.client.get?doctype=User&name=Administrator", auth_headers, None),
    ]
    for method, path, headers, data in reqs:
        try:
            if method == "GET":
                r = client.get(path, headers=headers)
            else:
                r = client.post(path, headers=headers, data=data)
            statuses.append(r.status_code)
        except Exception:
            statuses.append(-1)
    return statuses


def run_workload(rounds=ROUNDS, verbose=False):
    import frappe
    import frappe.app
    from werkzeug.test import Client

    frappe.init(site=SITE, sites_path=".")
    frappe.connect()
    frappe.set_user("Administrator")

    # api key/secret for authenticated WSGI requests
    from frappe.core.doctype.user.user import generate_keys
    api_secret = generate_keys("Administrator")["api_secret"]
    frappe.db.commit()
    api_key = frappe.db.get_value("User", "Administrator", "api_key")

    client = Client(frappe.app.application)
    auth_headers = {"X-Frappe-Site-Name": SITE,
                    "Authorization": f"token {api_key}:{api_secret}"}
    guest_headers = {"X-Frappe-Site-Name": SITE}

    summary = {"rounds": rounds}
    for r in range(rounds):
        summary["meta"] = _warm_meta(frappe)
        summary["reads"] = _orm_reads(frappe)
        summary["writes"] = _orm_writes(frappe)
        _templating(frappe)
        summary["wsgi"] = _wsgi_requests(frappe, client, auth_headers, guest_headers)
        if verbose:
            sys.stderr.write(f"[workload] round {r+1}/{rounds} done: {summary}\n")
            sys.stderr.flush()

    summary["n_modules"] = len(sys.modules)
    return summary


if __name__ == "__main__":
    s = run_workload(verbose=True)
    print("WORKLOAD-DONE", s)
