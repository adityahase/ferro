# 01 — HTTP differential results (ferro vs CPython oracle, per-test)

Same harness, same test files, run against a live **ferro** (:8081) and a live **CPython Frappe**
oracle (:8002). Source of truth: `ferro-test-harness/results/DIFF.md` + `results/json/*.json`.

## Headline counts (9 HTTP-capable modules)
| Bucket | Count | Meaning |
|---|---|---|
| **OK** (pass on both) | 49 | ferro matches |
| **FERRO_GAP** (oracle pass, ferro fail) | **15** | real ferro shortfalls ← the signal |
| **BOTH_BAD** (fail on both) | 59 | environment/harness artifacts, NOT ferro |
| **FERRO_BETTER** (ferro pass, oracle fail) | 6 | harness/test-ordering quirks |

Modules with **zero** ferro gaps: `test_client` (12/12), `test_cors` (4/4), `test_caching`
(13 OK / 2 env), `test_hooks` (3 OK / 3 env), `test_oauth20` (all env), `test_frappe_client`
(all masked by session limitation — see below).

## The 15 FERRO_GAPs, grouped by root cause

### G1 — Role resolution bug → Guest over-permitted (HIGH, security)  ✅ root-caused
`auth.rs:144 user_roles()` seeds `vec!["All"]` for **every** user including Guest, and never adds
"Guest" to logged-in users. Frappe: `Guest → ["Guest"]` only; others → `[<HasRole>, "All", "Guest"]`.
Effect: **Guest can read any doctype whose DocPerm grants role "All"** (ToDo, Note, …).
- `test_api_v2.TestResourceAPIV2.test_unauthorized_call` — Guest GET `/api/v2/document/ToDo` → ferro **200**, oracle **403**.
- (Also surfaces in `test_api.TestResourceAPI.test_get_doc_expand`, masked BOTH_BAD: Guest GET ToDo by name → ferro returns the doc.)
- Direct probe confirms: `GET /api/resource/ToDo` and `/ToDo/<name>` as Guest → **200** on ferro (should be 403). `GET /api/resource/User` as Guest → 403 (correct, no "All" perm on User). DB: `tabDocPerm(parent='ToDo')` grants read to role **All**.

### G2 — No session/sid auth → login-policy & cookie tests fail (architectural)  ✅ root-caused
ferro issues a random `sid` cookie on `login` but has **no session store** and never validates sid;
every request resolves from the token header or falls to `default_user`. The `login` method verifies
the password but ignores the System Settings login policies and sets no cookie expiry.
- `test_auth.test_allow_login_using_mobile / _only_email / _username` — login via mobile/username
  should be rejected/accepted per `allow_login_using_mobile_number` / `allow_login_using_user_name`;
  ferro ignores both → "AuthError not raised".
- `test_auth.test_disable_user_pass_login` — `disable_user_pass_login=1` should block password login;
  ferro ignores → login succeeds.
- `test_auth.test_deny_multiple_login` — `deny_multiple_sessions=1` should invalidate the older
  session; ferro has no sessions → "Exception not raised".
- `test_auth.test_correct_cookie_expiry_set` — the `sid` cookie has no `Expires`; `get_expiry...`
  is None → TypeError.
- Confirmed by probe: FrappeClient login → ferro **200 + sid cookie**, but the next `get_doc("User")`
  fails (runs as Guest); `get_list("ToDo")` only "works" because of G1.

### G3 — `expand` / `expand_links` not implemented (MEDIUM)  ✅ root-caused
List/get with `expand`/`expand_links` should replace a link value with the linked doc as a nested
dict; ferro returns the raw link string.
- `test_api.TestResourceAPI.test_get_list_expand` — `response["data"][0]["allocated_to"]` is
  `'test@restapi.com'`, expected a dict.

### G4 — v2 bulk operations missing (MEDIUM)  ✅ root-caused
ferro doesn't implement the v2 bulk endpoints.
- `test_bulk_delete_docs_single_doctype` / `_partial_failure` — POST `/api/v2/document/<dt>/bulk_delete` → **405**.
- `test_bulk_delete_cross_doctype` — POST `/api/v2/method/bulk_delete` → **404**.
- `test_bulk_delete_invalid_format` — should be **417** with `errors[0].exception` "'docs' must be a list"; ferro **404**.
- (bulk_update variants are BOTH_BAD: they enqueue background jobs, unsupported in this env on both.)

### G5 — Missing whitelisted methods (LOW–MEDIUM)  ✅ root-caused
- `test_api.TestMethodAPI.test_get_user_info` — `/api/method/frappe.realtime.get_user_info` → **404**;
  Frappe returns `{}` for server-to-server. Trivial stub.
- `test_api.TestAPIResponse.test_binary_and_csv_response` — POST
  `/api/method/frappe.core.doctype.data_import.data_import.download_template` → **404**. This is an
  arbitrary whitelisted **Python app method**; out of scope for pure ferro (handled by `ferrod`).

### G6 — `debug=1` → `_debug_messages` not added (LOW)  ✅ root-caused
- `test_api.TestResourceAPI.test_get_list_debug` — with `debug=true` the response should include a
  `_debug_messages` JSON string; ferro omits it. Cosmetic/dev-only.

## BOTH_BAD (59) — NOT ferro bugs (classified, with reasons)
- **Server-side writes (≈20)**: create/update/delete/copy/bulk-update across test_api & test_api_v2 →
  `DatabaseError` 500 on BOTH (SQLite write-lock contention with the in-process test txn). ferro's
  write path validated separately (verify.py + standalone probe = 200). See `00-methodology.md`.
- **FrappeClient suite (12)**: every `test_frappe_client` test errors on both — admin password masking
  + (on ferro) the session limitation G2. Not a ferro-vs-oracle signal here.
- **OAuth (6)**, **read-only mode (2)**, **PDF/print (`test_generate_pdf`)**, **custom-app hooks
  (`test_hooks` 3: auth_hook/has_permission/override_doctype_class need a test app's hooks.py)**,
  **`test_logs`/`test_array_response`/`test_request_hooks`/`test_login_redirects`** — all require
  subsystems/setup absent in this single-app SQLite bench; fail identically on both.

## FERRO_BETTER (6) — harness quirks, not real wins
e.g. `test_404s`, `test_ping`, `test_get_list_fields` pass on ferro but error on oracle due to
test-ordering interaction with the rotating admin key / oracle's stricter field handling under the
token bridge. Not ferro correctness claims.

## Per-module gap index
| module | OK | FERRO_GAP | BOTH_BAD | gaps |
|---|---|---|---|---|
| test_api | 7 | 4 | 12 | G1,G3,G5(×2),G6 |
| test_api_v2 | 3 | 5 | 22 | G1, G4(×4) |
| test_auth | 4 | 6 | 2 | G2(×6) |
| test_caching | 13 | 0 | 2 | — |
| test_client | 12 | 0 | 0 | — |
| test_cors | 4 | 0 | 0 | — |
| test_frappe_client | 1 | 0 | 12 | masked (G2) |
| test_hooks | 3 | 0 | 3 | — |
| test_oauth20 | 2 | 0 | 6 | — |
