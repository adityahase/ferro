## rest-envelopes

Domain: exact success/error envelope shapes, status codes, and content-type for the v1
(`/api/method`, `/api/resource`) and v2 (`/api/v2/document`, `/api/v2/method`, `/api/v2/doctype`)
REST surfaces. Spec tests: `frappe/tests/test_api.py` (v1) and `frappe/tests/test_api_v2.py` (v2).

Judgment rubric: **1:1** — this is core REST contract ferro explicitly reimplements (`err`/`err_v2`/
`route_resource`/`route_v2_document`). Substantive envelope/status assertions MUST match. Python-only
whitelisted method bodies (`frappe.tests...test`, `test_array`, `get_all_roles`) are *out of scope*
for pure ferro (need the desk registry / Python), but the **error/envelope shape** they exercise is
in scope and is validated via ferro's own error paths.

All probes below were run against the live ferro server
`ferro serve mysite.sqlite --desk --default-user Guest -b 127.0.0.1:8081` with a freshly provisioned
Administrator token. ferro builds in production mode here (`app.dev == false`).

### Behaviors Frappe guarantees (bullet list)

V1 (`frappe/api/v1.py`, `frappe/utils/response.py`):
- Success list/read/create/update: `{"data": <doc|list>}`, HTTP 200.
- DELETE: HTTP **202**, body exactly `{"data": "ok"}` (`delete_doc` sets `http_status_code = 202`, returns `"ok"`). (`test_delete_document`)
- Method call success: `{"message": <return value>}`, HTTP 200 (`handler.handle` → `frappe.response["message"]`). (`test_ping` → `{"message":"pong"}`, `test_array_response`)
- Unauthorized read with no/invalid perms → **403** PermissionError. (`test_unauthorized_call`)
- Invalid/missing API credentials → **401** AuthenticationError. (`test_auth_cycle`)
- Unknown `/api/<x>` path → **404**; nonexistent doc → **404 DoesNotExistError**. (`test_404s`)
- Error envelope V1 = top-level keys `{"exc_type", "exception"?, "exc"?, "_server_messages"}`:
  - `exc_type` = exception class name (e.g. `PermissionError`, `ValidationError`, `DoesNotExistError`).
  - `exception` = last traceback line, **only** when traceback is allowed (dev / system-user). (`test_logs`)
  - `exc` = JSON-encoded list of traceback strings, dev-only. (`test_logs`)
  - `_server_messages` = `orjson.dumps([orjson.dumps(d) for d in message_log])` — a **JSON string whose value is a JSON array of JSON-encoded message dicts** (double-encoded). Each dict has `message`/`title`. (`test_run_doc_method`, `test_logs`)
  - The 403 PermissionError envelope is EXACTLY `{exc_type, exception, exc, _server_messages}` (4 keys). (`test_run_doc_method`)
- HTTP-method→ptype for resource ops: GET=read, POST=create, PUT/PATCH=write, DELETE=delete (werkzeug route endpoints). For `execute_doc_method`/run_doc_method: GET→`read`, POST→`write`.
- `mimetype = "application/json"` for all JSON responses (`as_json`).
- `as_dict=False` list → rows are **lists** (positional), `as_dict=True/default` → rows are **dicts**. (`test_get_list_dict`)
- `limit`/`limit_page_length` honored; default 20. (`test_get_list_limit`)

V2 (`frappe/api/v2.py`, `frappe/utils/response.py` ApiVersion.V2):
- Success: `{"data": <...>}`, 200. Lists ALSO carry top-level `{"has_next_page": bool}` (computed by fetching `limit+1`). (`test_get_list`, `document_list`)
- DELETE: **202** `{"data": "ok"}`. (`test_delete_document`)
- Method success: `{"data": <return>}`; `ping` → `{"data":"pong"}`; `get_logged_user` → `{"data":"Administrator"}`. (`test_ping`, `test_auth_cycle`)
- Error envelope V2 = `{"errors": [ {"type": <ClassName>, "message"?, "exception"? , ...message-log keys} ]}`. `exception` (full traceback) added only when traceback allowed; for 404 there is NO `exception`. (`test_delete_document_non_existing`, `test_logs`)
- Message log surfaced as top-level `{"messages": [ {message,...} ]}` (not `_server_messages`). (`test_logs`)
- ptype map `{GET:read, POST:write}` for `execute_doc_method`/`run_doc_method` (PERMISSION_MAP).
- `/api/v2/doctype/<dt>/meta` → `{"data": <meta>}` with `data.name == dt`; `/count` → `{"data": <int>}`. (`test_meta`, `test_count`)
- `/api/v2/document/<dt>/<name>/copy` → 200 clean copy (no name/creation/owner/docstatus). (`test_copy_document`)
- `/api/v2/document/<dt>/<name>/method/<m>` → run controller method. (`test_execute_doc_method`)
- Bulk: `bulk_delete`/`bulk_update` (cross-dt via `/method/`, single-dt via `/document/<dt>/bulk_*`): success `{"data":{deleted/updated, failed, total, success_count, failure_count}}`; invalid top-level `docs`/`names` not a list → **417** with `errors[0].exception` containing `"'docs' must be a list"`; >threshold (20) → **202** `{"data":{"job_id":...}}`. (`TestBulkOperationsV2`)
- Read-only/maintenance mode: writes → **503** with V1 `exc_type=="InReadOnlyMode"` / V2 `errors[0].type=="InReadOnlyMode"`. (`TestReadOnlyMode`)

### MATCH (ferro replicates)

1. **V1 success envelope `{"data":...}`** for list/read — `main.rs:1373`, `main.rs:1382`. Probe `GET /api/resource/ToDo?limit=2` → `{"data":[...]}`. (`test_get_list`, `test_get_list_fields`)
2. **V1 DELETE → 202 `{"data":"ok"}`** — `main.rs:1425`. Probe: create+delete returned `HTTP/1.1 202` body `{"data":"ok"}`. (`test_delete_document`)
3. **V2 DELETE → 202 `{"data":"ok"}`** — `main.rs:1191`. (`test_delete_document` v2)
4. **V1 method success `{"message":X}`** — `route_method` `main.rs:1308` (`ping`→`{"message":"pong"}`), and CRUD `{"data"}` vs method `{"message"}` distinction is correct. Probe `/api/method/ping` → `{"message":"pong"}`. (`test_ping`)
5. **V2 method success `{"data":X}`**: `ping`→`{"data":"pong"}` (`main.rs:1218`), `get_logged_user`→`{"data":"Administrator"}` (`main.rs:1221`), `login`→`{"data":"Logged In"}` (`main.rs:1219`), `logout`→`{"data":null}` (`main.rs:1220`). Probes confirm. (`test_ping`, `test_auth_cycle`)
6. **V1 error envelope** has `{"exc_type", "_server_messages"}` (and dev-only `exception`) — `err` `main.rs:891-904`. The 403 PermissionError probe returned `{"exc_type":"PermissionError","_server_messages":"[...]"}`. In prod (no traceback) Frappe omits `exception`/`exc`/`exc_source` too, so the 2-key shape is consistent with Frappe-prod. (`test_run_doc_method`, `test_unauthorized_call`)
7. **`_server_messages` double-JSON shape** — `err` `main.rs:892-896` builds `to_string(vec![to_string(&{message,title})])`, i.e. a JSON string of a JSON array of JSON-encoded `{message,title}` dicts. This is structurally identical to Frappe's `orjson.dumps([orjson.dumps(d)...])` and survives the test's triple `parse_json(...)[0]` decode. (`test_logs` shape; `test_run_doc_method`)
8. **V2 error envelope `{"errors":[{type,message,exception?}]}`** — `err_v2` `main.rs:922-930`, `map_orm_err_v2` `main.rs:933-943`. Probe v2 delete-nonexistent → `{"errors":[{"type":"DoesNotExistError","message":"ToDo non-existent-xyz123"}]}`, **no** `exception` key in prod — exactly matches `test_delete_document_non_existing` (`errors[0].type=="DoesNotExistError"`, `assertFalse(errors[0].get("exception"))`).
9. **V2 list `has_next_page`** — `route_v2_document` over-fetches `limit+1` and reports `has_next_page` (`main.rs:1126-1138`). Probe `GET /api/v2/document/ToDo?limit=2` → `{"data":[..2..],"has_next_page":true}`. (`document_list`)
10. **Status code mapping for ORM errors** — `map_orm_err`/`map_orm_err_v2`: NotFound→404 DoesNotExistError, Validation→417 ValidationError, Duplicate→409 DuplicateEntryError, Db→500 DatabaseError (`main.rs:906-917`, `933-943`). 404/417/409 match Frappe's status codes. (`test_404s`, dup/mandatory implied)
11. **Unauthorized read → 403** — `route_resource`/`route_v2_document` permission gate (`main.rs:1345`, `1096`). With `--default-user Guest`, no-auth `GET /api/resource/User` → 403, and `GET /api/v2/document/User` → 403. (`test_unauthorized_call` v1 & v2)
12. **Invalid credentials → 401** — `route` `main.rs:984-988` returns 401 AuthenticationError on `AuthOutcome::Unauthorized`. Probe: bad secret and nonexistent key both → 401. (`test_auth_cycle`)
13. **Unknown path → 404 / nonexistent doc → 404** — `main.rs:1003/1059` (unknown path) and `map_orm_err` NotFound 404. Probe `/api/rest`→404, `/api/resource/User/NonExistent@s.com`→404 DoesNotExistError. (`test_404s`)
14. **HTTP-method→ptype mapping** — `auth::ptype_for_method` `auth.rs:119-127`: GET/HEAD→read, POST→create, PUT/PATCH→write, DELETE→delete. Matches v1 werkzeug endpoint semantics. (CRUD perm tests)
15. **405 MethodNotAllowed** — unmatched (method,name) combos return 405 `{"exc_type":"MethodNotAllowed",...}` / v2 `errors[0].type=="MethodNotAllowed"` (`main.rs:1430`, `1196`). Probe `PUT /api/resource/ToDo` (no name) → 405. (not directly tested but correct)
16. **`limit`/`limit_page_length` honored, default 20** — `build_list_query` `main.rs:1466-1472`. (`test_get_list_limit`)

### PARTIAL

P1. **V1 `run_method` / `execute_doc_method` on `/api/resource/<dt>/<name>?run_method=...`** — `test_run_doc_method` (v1). ferro does NOT execute the method; it adds `run_method` to the body-skip list (`main.rs:1487`) but the GET-with-name branch just calls `orm::get_doc` and returns the FULL DOC as `{"data": <dict>}`. Probe `GET /api/resource/Website Theme/Standard?run_method=get_apps` → 200 but `data` is a doc dict, not the method's list result. The test allows status ∈ {403, 200}; ferro returns 200 but then `assertIsInstance(data, list)` would FAIL (ferro returns a dict). So the envelope shape (200, `{"data":...}`) is right but the semantics are wrong. PARTIAL (Med).

P2. **`as_dict=False` not honored (rows always dicts)** — `test_get_list_dict` (v1 & v2). ferro never reads `as_dict`/`as_list` (no reference in `orm.rs`/`main.rs build_list_query`); `orm::get_list` always returns an array of dicts. Probe `as_dict=False` → `{"data":[{"name":...}]}` (dicts), but the test asserts `data[0]` is a **list**. The `as_dict=True` branch passes; the `as_dict=False` branch FAILS. PARTIAL (Low — niche legacy param). Fix: in `build_list_query` parse `as_dict` (default true); when false, project each row dict to a positional list in field order. Also affects v2.

### GAP

G1. **V2 auth-error (401) returns the V1 envelope, not V2** — Severity **Med**. `route()` builds the 401 BEFORE dispatching by version: `main.rs:987` calls `err(app.dev, 401, "AuthenticationError", ...)`, which yields `{"exc_type","_server_messages"}` for *every* path including `/api/v2/...`. Probe `GET /api/v2/document/ToDo` with `Authorization: token bad:bad` → `{"exc_type":"AuthenticationError","_server_messages":"[...]"}` (V1 shape) instead of `{"errors":[{"type":"AuthenticationError",...}]}`. Not asserted by the supplied tests (which only check `status==401`), but it is a real envelope divergence that a v2 client (frappe-ui) reading `response.errors` would mishandle. **Fix location:** `main.rs:984-989`. **Sketch:** detect the v2 prefix from `segments` (`segments.get(1)=="api"? no — segments[1]=="api"? actually segments[0]=="api", segments[1]=="v2"`) before resolving auth, and emit `err_v2(...)` for v2 paths. Simplest: move the auth resolution below the version branch, or pass an `is_v2` flag into a small helper that picks `err` vs `err_v2`.

G2. **`/api/v2/doctype/<dt>/meta` and `/count` → 404** — Severity **Med**. `route()` only branches v2 on `segments[2] ∈ {"document","method"}` (`main.rs:1033-1058`); `"doctype"` falls to the `_ => err_v2(...404 NotFound)` arm. Probes: `/api/v2/doctype/ToDo/meta` → 404 `{"errors":[{"type":"NotFound",...}]}`, `/count` → 404. Breaks `TestDocTypeAPIV2.test_meta` (expects `data.name=="ToDo"`) and `test_count` (expects `data` int). **Fix location:** add a `Some("doctype")` arm in the v2 match at `main.rs:1033`. **Sketch:** parse `segments[3]=<dt>`, `segments[4] ∈ {"meta","count"}`; for `meta` load `app.metas.get` and serialize (`only_for("All")` is satisfied since any logged-in user has All); for `count` run a `SELECT COUNT(*)` via orm with the same perm gate → `{"data": n}`. Both must honor read permission and return `err_v2` on miss.

G3. **V2 bulk operations entirely missing** — Severity **Med** (delete) / **Low** (update needs enqueue). `route_v2_method` has no `bulk_delete`/`bulk_update` and no `/document/<dt>/bulk_delete` path; the `bulk_delete` segment is treated as a doc *name* in `route_v2_document` (`main.rs:1082` `name = segments[4..]`), and the dotted `/api/v2/method/bulk_delete` falls through to 404. Probes: `POST /api/v2/method/bulk_delete` → 404 NotFound; invalid-format case returns 404 not the required **417 `'docs' must be a list'`**. Breaks all of `TestBulkOperationsV2` (single/cross dt delete+update, partial-failure, invalid-format 417, >20 enqueue 202). **Fix location:** new arms in `route_v2_method` (`main.rs:1217`, for the bare `bulk_delete`/`bulk_update`) and in `route_v2_document` POST when `name=="bulk_delete"|"bulk_update"` (`main.rs:1112`). **Sketch:** parse `docs`/`names` from body; if not a list → `err_v2(417,"ValidationError","'docs'/'names' must be a list")`; else loop with per-item validation (`'name' must be a string or integer`, `must be a dictionary`), accumulate `{deleted/updated, failed:[{name,error}], total, success_count, failure_count}` → `{"data": that}`. Skip the >20-threshold enqueue path (no Python jobs for arbitrary doctypes; the two enqueue tests are "somewhat" scope).

G4. **V2 `execute_doc_method` (`/api/v2/document/<dt>/<name>/method/<m>`) → 404** — Severity **Low** (Python controller method). Resolution only works under `--features python` via `pyfall.resolve_doctype_method` (`main.rs:1040-1051`); in pure ferro `route_v2_document` treats `method/get_apps` as part of the doc `name` → `get_doc` 404. Probe → 404. Breaks `test_execute_doc_method`. This needs real controller code, so it is `shouldn't-care` for pure ferro (ferrod path) — recorded as a GAP for completeness.

G5. **V2 `copy` (`/api/v2/document/<dt>/<name>/copy`) → 404** — Severity **Low**. No `copy` handling; `copy` is swallowed into the doc name → 404. Probe → 404. Breaks `test_copy_document`. **Fix location:** in `route_v2_document` GET-with-name branch, when the last segment is `copy`, return `get_doc` minus the no-copy/audit fields (`name, owner, creation, modified, modified_by, docstatus`) as `{"data":...}`. Med effort; Low priority.

G6. **Read-only / maintenance mode (503 InReadOnlyMode) not implemented** — Severity **Low**. No `maintenance_mode`/`InReadOnlyMode`/503 anywhere in `main.rs`/`orm.rs`. `TestReadOnlyMode` (v1 & v2) `test_blocked_writes*` expect 503 with `exc_type/errors[0].type == "InReadOnlyMode"`. Per the shared context this is also a harness-config item (`update_site_config maintenance_mode`), so likely a `somewhat`/`shouldn't-care` gap, but it is a missing envelope+status. **Fix:** read `maintenance_mode` from site_config at startup; on write methods (POST/PUT/PATCH/DELETE) when set and `allow_reads_during_maintenance`, short-circuit with `err(503,"InReadOnlyMode",...)` / `err_v2(...)`.

G7. **Python whitelisted method bodies → 404** (`frappe.realtime.get_user_info`, `frappe.tests.test_api.test`/`test_array`, `run_doc_method`, shorthand `User/get_all_roles`) — Severity **Low**, mostly `shouldn't-care`. Probes all 404. Notable single in-scope one: **`frappe.realtime.get_user_info` should return `{"message":{}}` (v1) / `{"data":{}}` (v2)** (`test_get_user_info`); trivially addable as a stubbed method in `ferro_method`/`route_method`. The `test`/`test_array`/`run_doc_method`/`get_all_roles` cases require real Python or the in-memory doc runner and are out of scope for pure ferro (ferrod path). For these, the **error envelope** ferro returns (404 `exc_type:NotFound` / `errors[0].type:NotFound`) is itself correctly shaped.

### Undocumented ferro behavior discovered (divergences not covered by a test)

U1. **`--desk` silently flips `default_user` to Administrator** — `main.rs:601-602`: when `--desk` is set and `--default-user` is NOT given, the anonymous/no-auth user becomes **Administrator**, not Guest. That would make `test_unauthorized_call` return **200** (Administrator can read User) instead of 403, i.e. an effective auth bypass for unauthenticated requests. The deployed harness avoids this only because it passes `--default-user Guest` explicitly. Not covered by any test; security-relevant default.

U2. **JSON content-type is `application/json; charset=utf-8`** (ferro, `desk::RawResp::json`) vs Frappe's `application/json` (no charset). Harmless and not asserted by these tests, but a literal-header divergence.

U3. **`run_method` GET silently returns the doc instead of erroring** — see P1; rather than 404/"method not run", ferro returns a normal `{"data": <doc>}` 200, which could mislead a v1 client expecting the method result.

U4. **ORM doc serialization corruption on some doctypes** — incidental to envelope probing: `GET /api/resource/Website Theme/Standard` returned keys like `"\"parent\"":"parent"`, `"\"parentfield\"":"parentfield"` (quoted column names round-tripped into the dict). Not an envelope bug (the `{"data":...}` wrapper is correct) but a data-integrity divergence worth flagging to the ORM-domain agent.

U5. **`v1_to_v2` re-shaping path for method errors** (`main.rs:949-972`) correctly extracts the human message from v1's `_server_messages` double-JSON when down-converting desk method errors to v2 — a faithful detail, though only reachable for desk/ferrod methods, not the core CRUD path.
