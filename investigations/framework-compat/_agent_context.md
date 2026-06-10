# Shared context for ferro ↔ Frappe compatibility analysis agents

## What ferro is
ferro is a **pure-Rust runtime** that serves the **Frappe REST API + Desk + app SPAs** against an
existing Frappe **SQLite** site, replacing the CPython+Frappe gunicorn worker. Goal: <64MB RAM.
It reads/writes the SAME `tab<DocType>` tables Frappe created. It is NOT a Python module — the only
way to exercise it is over HTTP. A `--features python` build (`ferrod`) can fall through to real
Python for whitelisted app methods; the DEFAULT build is pure Rust and is the deliverable under test.

Design priorities (from the project): **token-auth REST data plane**, minimal footprint, *do not
reimplement unnecessary parts*. So whole Frappe subsystems are intentionally absent.

## ferro source map  (/home/frappe/ferro/src/)
- `main.rs` (1537) — HTTP server, routing: `/api/method`, `/api/resource` (v1), `/api/v2/document`,
  `/api/v2/method`. `route()`, `route_resource()`, `route_method()`, `route_v2_document()`,
  `route_v2_method()`, `build_list_query()`, `build_doc_data()`, `attach_login_cookies()`.
- `orm.rs` (988) — the ORM: `get_list`, `get_doc`, `insert`, `update`, `delete`, `doc_owner`,
  child tables, filters. Analogue of frappe.client / frappe.model.db_query.
- `auth.rs` (272) — token/Basic auth + permission gate. `resolve_user`, `verify_token`, `user_roles`,
  `permission`, `readable_permlevels`, `owns`, `ptype_for_method`. **NO session/sid auth.**
- `meta.rs` (253) — DocType meta from tabDocType/tabDocField, cached.
- `naming.rs` (261) — autoname rules (naming_series, field:, format:, hash, expression, etc.).
- `schema.rs` (623) — doctype JSON ⇄ SQLite DDL (used by bench install/migrate, not the web path).
- `crypto.rs` (683) — Fernet decrypt, pbkdf2, md5, hmac (token verify, password).
- `desk.rs` (1442) — Desk SPA shell + `desk::route_method` implementing many `frappe.*` whitelisted
  methods (frappe.client.get/get_list/get_value/get_count/insert/save, frappe.desk.form.load.*,
  frappe.desk.reportview.*, frappe.client.get_single_value, login, logout, etc.). Active only with `--desk`.
- `realtime.rs`/`jobs.rs`/`cache.rs` — in-process socket.io / RQ / redis replacements (bench-mode).
- `spa.rs`, `install.rs`, `newsite.rs`, `bench.rs`, `pyfall.rs`, `util.rs`.

## Method surface (pure ferro, with `--desk`)
`/api/method` resolves in order: ferro internal methods → `desk::route_method` (frappe.client.*,
frappe.desk.*) → `route_method` (ping, frappe.auth.get_logged_user). Anything else → **404 NotImplemented**.
`/api/resource` + `/api/v2/document` = full CRUD via orm.rs. `/api/v2/method` delegates.

## EMPIRICAL FINDINGS (HTTP differential: same test run vs live ferro AND live CPython "oracle")
Confirmed ferro shortfalls (oracle passes, ferro fails) and harness artifacts:

### Confirmed ferro gaps
1. **Role bug** — `auth.rs:144 user_roles()` seeds `vec!["All"]` for EVERY user incl **Guest**, and
   never adds "Guest" to logged-in users. Frappe: Guest→`["Guest"]` only; others→`[HasRole…,"All","Guest"]`.
   Effect: **Guest can read any doctype whose DocPerm grants role "All"** (e.g. ToDo). Causes
   test_api_v2 `test_unauthorized_call` (200 vs 403) and Guest reads that should be 403. HIGH/security.
2. **No session/sid auth** — ferro issues a random `sid` cookie on login but has no session store and
   never validates sid. Every request resolves user from the token header or falls to `default_user`.
   Cookie-session clients (FrappeClient password login, browser non-admin Desk) cannot authenticate;
   subsequent calls run as default_user. MAJOR architectural divergence (likely by design).
3. **Login policy not enforced** — the `login` method verifies the password but ignores System Settings
   `allow_login_using_mobile_number`, `allow_login_using_user_name`, `disable_user_pass_login`,
   `deny_multiple_sessions`; login sid cookie has no Expires. Causes 6 test_auth failures.
4. **`expand` / `expand_links` not implemented** — list/get returns raw link value instead of the
   expanded child dict. Causes test_get_list_expand.
5. **`debug=1` → `_debug_messages`** not added to responses. test_get_list_debug. LOW.
6. **v2 bulk operations missing** — `/api/v2/document/<dt>/bulk_delete`, `/api/v2/method/bulk_delete`,
   bulk_update → 404/405. test_bulk_* (delete is testable; update needs enqueue).
7. **Missing whitelisted methods** — `frappe.realtime.get_user_info` (→404, should be `{}`),
   `frappe.core.doctype.data_import...download_template` (Python app method → N/A for pure ferro).

### Harness artifacts (NOT ferro bugs — both oracle & ferro fail identically)
- **Write contention**: the in-process test harness holds a SQLite write lock during each test;
  any server-side write hits SQLITE_BUSY → DatabaseError 500 on BOTH targets. So ALL server-write
  tests (create/update/delete/copy/bulk-update) are un-runnable through this harness. ferro's write
  path is validated separately by `measurements/verify.py` + direct probes (standalone create = 200).
- OAuth, read-only mode, PDF generation, background-job enqueue, custom-app hooks: not set up in this
  single-app SQLite bench → fail on BOTH.
- FrappeClient password login masked unless admin password is set.

## Bench under test
`/home/frappe/benches/bench-cpython314` — Frappe v17 (17.0.0-dev), SQLite site `mysite.sqlite`,
ONLY the `frappe` app installed. (No erpnext/crm/etc.) Test copies: `bench-test` (ferro), `bench-oracle` (cpython).
Test sources: `/home/frappe/benches/bench-cpython314/apps/frappe/frappe/`.

## Judgment rubric — for each test class, rate ferro's obligation to match:
- **1:1** — core REST/ORM/auth/perm/naming contract ferro explicitly reimplements; MUST match
  (CRUD, filters, token auth, permission masking, naming, error codes/envelopes).
- **mostly** — same domain, but some sub-behaviors are acceptable to differ (e.g. debug payloads,
  minor response extras) — match the substantive assertions.
- **somewhat** — partially in scope; ferro should do *something* compatible but full fidelity is
  optional given footprint goals (sessions/login policy, expand, v2 bulk, realtime helpers).
- **shouldn't care** — tests a Python/app subsystem ferro intentionally does NOT implement
  (email, workflow engine, reports/print, oauth, server scripts, background jobs in Python,
  data import, social login, web forms, etc.) OR pure in-process Python internals never exposed
  over HTTP (test of a util function, a Python class, etc.). For these, ferrod (python fallthrough)
  is the answer, not pure ferro.

Note: a test being "in-process only" (calls frappe.* directly, never HTTP) means it does not exercise
ferro AT ALL — but its ASSERTIONS may still encode an ORM/perm/naming behavior ferro reimplements and
must match. Distinguish "exercises ferro over HTTP" from "encodes a behavior ferro must replicate".
