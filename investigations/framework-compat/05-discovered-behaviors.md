# 05 — Discovered behaviors & divergences (things to know about ferro)

Behaviors uncovered during this audit that are NOT obvious from a single test — divergences between
ferro and Frappe that matter for compatibility decisions. (Behavioral-domain GAPs from the deep-dive
are in `04-behavioral-findings.md`; this file is the cross-cutting / architectural set.)

## D1 — ferro grants the "All" role to Guest (and omits "Guest"/"Desk User" from users)  — BUG
`auth.rs:144 user_roles()`:
```rust
let mut roles: Vec<String> = vec!["All".to_string()];   // every user, incl Guest
if user == "Guest" { roles.push("Guest".to_string()); }
// + Has Role rows
```
Frappe `frappe/permissions.py get_roles()` is:
- `Guest` → `["Guest"]` **only** (no "All").
- `Administrator` → all roles.
- normal user → `<Has Role minus AUTOMATIC_ROLES>` + `["All","Guest"]` + `["Desk User"]` if
  `user_type=="System User"`.
Constants: `ALL_USER_ROLE="All"`, `GUEST_ROLE="Guest"`, `SYSTEM_USER_ROLE="Desk User"`,
`AUTOMATIC_ROLES=("Guest","All","Desk User","Administrator")`.

**Consequences**
- Guest can read/anything any doctype whose DocPerm grants role **"All"** (ToDo, Note, Comment,
  many core doctypes). Confirmed: Guest `GET /api/resource/ToDo` and `/ToDo/<name>` → 200 on ferro.
  This is a **security-relevant over-permission**.
- Logged-in non-admin users are missing the implicit **"Guest"** and **"Desk User"** roles, so
  doctypes that grant perms only to those roles would be wrongly denied to real users. (Less visible
  because most real users also hold explicit roles, but it is still incorrect.)

## D2 — ferro has no session / sid authentication  — ARCHITECTURAL
ferro authenticates **per request** from the `Authorization` header (token/Basic) only. With no
header it uses a server-wide `default_user` (`Guest` in REST mode, `Administrator` in `--desk` mode
unless `--default-user` is passed). On `login` it sets a **random** `sid` cookie via
`main.rs:858 attach_login_cookies()` that is never stored and never validated.

**Consequences**
- Any cookie/session client breaks: `FrappeClient(url, user, pw)` logs in (ferro returns 200 + a sid
  cookie) but every subsequent call runs as `default_user`, not the logged-in user. Confirmed:
  after login, `get_doc("User","Administrator")` fails (Guest), while `get_list("ToDo")` "succeeds"
  only because of D1.
- Browser Desk works today only because ferro is launched with `default_user=Administrator` — i.e.
  *everyone* is Administrator. Multi-user / per-user Desk sessions are not possible.
- This is plausibly **by design** (token-first, low-footprint), but it is the single largest source
  of test divergence (all of `test_auth`'s policy tests, the whole `test_frappe_client` suite, and
  any session-expiry/CSRF test). Decision needed: implement a minimal `tabSessions`-backed sid
  validator, or formally scope sessions out (and document that clients must use tokens).

## D3 — `login` is a non-authenticating stub (accepts ANY credentials)  — IMPORTANT
`desk.rs:583` — the `login` method returns `(200, {"message":"Logged In", ...})` **unconditionally**:
it does NOT verify the password, does NOT resolve the `usr`, and echoes `default_user` as `full_name`.
Confirmed by probe: `POST /api/method/login {"usr":"Administrator","pwd":"totally-wrong"}` → 200
`{"message":"Logged In","full_name":"Guest"}`; a nonexistent user → same. The cookies it sets
(`sid=<random>`, `user_id=<default_user>`, `system_user=yes`, `full_name`) describe the *default*
user, not the credentialed one.
Consequences: (a) password login is effectively a no-op — any client "logs in" but is still
`default_user` on the next request (compounds D2); (b) the `allow_login_using_mobile_number`,
`allow_login_using_user_name`, `disable_user_pass_login`, `deny_multiple_sessions` System Settings are
all ignored; (c) the sid cookie has no `Expires`/`Max-Age`. This is why every `test_auth` login-policy
test diverges. **Decision needed**: either make `login` actually verify credentials + mint a real
session (couples with D2), or remove the cosmetic `login`/cookies so clients are pushed to token auth
and the endpoint can't be mistaken for real authentication.

## D4 — Server-side writes are fine standalone; the 500s in the differential are harness-only
A standalone authenticated `POST /api/resource/ToDo` returns **200** and persists. The create/update
/delete 500s seen in the HTTP differential are caused by the in-process test framework holding a
SQLite write lock while the separate server process tries to write (`SQLITE_BUSY` → `DatabaseError`).
This affects the CPython oracle identically. ferro's write semantics are validated by `verify.py`.
Not a ferro defect — but worth recording so the 500s aren't misread.

## D5 — `--desk` changes the default user to Administrator (auth posture flips with a flag)
`main.rs:601` — when `--desk` is set and `--default-user` is not, `default_user` becomes
`Administrator`. So `ferro serve --desk` (no `--default-user`) authenticates *every* unauthenticated
request as Administrator. For an internet-facing deployment this is effectively "auth off." The test
harness deliberately passes `--default-user Guest` to restore correct semantics. This coupling
(presentation flag silently changing the security posture) is a sharp edge worth a guardrail.

## D6 — Method surface is allowlist-by-implementation; unknown methods → 404 NotImplemented
`/api/method/<x>` only answers for the handful implemented in `ferro_method` + `desk::route_method`
+ `route_method`; everything else is `404 {"exc_type":"NotFound", ... "Method '<x>' not implemented"}`.
Frappe would instead resolve any `@frappe.whitelist()` across installed apps. For pure ferro this is
expected (the `ferrod` python-fallthrough tier closes it), but it means a frappe-ui frontend that
calls an app method ferro hasn't ported gets a 404 rather than a real result. (The set of methods
frappe-ui actually calls is the practical compatibility target — see `04`.)

## D8 — `frappe.client.*` read methods BYPASS permissions (data leak)  — 🔴 HIGH / security
The desk dispatch `desk::route_method` receives only `user: &str`, not the resolved permission
context, and the `frappe.client.get` / `get_list` / `get_value` / `get_count` / `get_single_value`
arms all read with `ReadAcl::all()` and **never call `auth::permission`** (`desk.rs:760,792,1022,1047`;
`get_single_value` reads `tabSingles` directly with no gate, `desk.rs:1063`). So the
`/api/method/frappe.client.*` path is an **unauthenticated read bypass** for *every* doctype —
independent of (and worse than) the Guest-`All` bug D1, which only exposed `All`-permitted doctypes.
**Confirmed by probe (as Guest, no auth):**
```
GET /api/resource/User                              → 403   (correct, gate enforced)
GET /api/method/frappe.client.get_list?doctype=User → 200   [{"name":"...@example.com"}, ...]
GET /api/method/frappe.client.get_value?doctype=User&fieldname=email&filters={} → {"email":"...@example.com"}
```
i.e. Guest reads **User email addresses**. The `/api/resource` + `/api/v2/document` CRUD paths DO
enforce permissions (`auth::permission` in `route_resource`/`route_v2_document`); only the
`frappe.client.*` *desk-method* path bypasses them. This path is active whenever ferro runs with
`--desk` (the Desk + SPA + signup deployments). **Most severe finding in this audit.** → FIX-9.
(Also: `frappe.client.*` *write* methods — `set_value`, `delete`, `submit`, `cancel`, `bulk_update`,
`validate_link_and_fetch`, `get_password`, `insert_many` — are simply **404 not implemented**.)

## D7 — Corrupt `parent`/`parentfield`/`parenttype` keys in single-doc GET (146 doctypes)  — BUG
`meta.rs:12 STANDARD_COLUMNS` lists `parent`,`parentfield`,`parenttype` as columns on *every* doc
table, and `meta.rs:156` seeds `meta.columns` from it unconditionally before adding the PRAGMA
(authoritative) columns. But those three columns exist only on **child** tables (`istable=1`).
For the **146 of 239** non-child tables that lack them (incl. **User, DocType, Role, Custom Field,
Dashboard…**), `get_doc` (`orm.rs:418`) emits `SELECT "parent", "parentfield", "parenttype" …`, and
SQLite's notorious *double-quoted-identifier-falls-back-to-string-literal* misfeature returns the
literal string with a quoted column name. Result, confirmed on `GET /api/resource/User/Administrator`:
```json
{ ... "\"parent\"": "parent", "\"parentfield\"": "parentfield", "\"parenttype\"": "parenttype" }
```
i.e. three bogus keys (with embedded quotes) whose values are the column names instead of `null`.
- **Why the differential missed it:** the create/update tests 500'd on write-contention, and the GET
  assertions check specific fields, not the *absence* of junk keys — so it passed unnoticed. Found
  only by the behavioral deep-dive + direct probe.
- **Severity:** Medium (correctness/cleanliness — a strict schema-validating client chokes; the values
  are wrong). **Fix:** see FIX-8 in `06-fix-plan.md` (don't seed child-only columns for non-child
  tables; trust PRAGMA for physical tables).

<!-- APPEND: additional cross-cutting divergences surfaced by the behavioral deep-dive workflow -->
