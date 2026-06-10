# client-methods — behavioral fidelity (Frappe `frappe.client.*` ↔ ferro)

Domain: the `frappe.client.*` whitelisted RPC surface (`/api/method/frappe.client.*`).
Spec: `frappe/tests/test_client.py`, `frappe/tests/test_client_cache.py`.
Source of truth for behavior: `frappe/client.py`.
ferro impl: `src/desk.rs::route_method` (dispatch at `desk.rs:581-650`), backed by `src/orm.rs`.
ferro CRUD also reachable via `/api/resource` + `/api/v2/document` (`main.rs`), but those are NOT
the `frappe.client.*` method path the tests exercise.

Probes were run against live ferro at `127.0.0.1:8081` (token auth; note `provision-key` ROTATES the
api_secret on every call, so each probe batch re-provisions — a stale token yields `AuthenticationError`,
which is not a method bug).

## client-methods

### Behaviors Frappe guarantees (from the spec tests + client.py)

- **`get_list(doctype, fields, filters, or_filters, order_by, group_by, limit_start, limit_page_length=20, as_dict, debug, expand)`** — list of dicts; `validate_args` runs; `expand`/`expand_links` expands link fields. (`test_array_values_in_request_args`)
- **`get(doctype, name=None, filters=None)`** — returns `doc.as_dict(no_nulls=True)` (null fields stripped); `name` takes precedence; if `filters` (incl. `{}`) parse them; else treat as Single. `filters` accepts a dict OR a JSON string and they're equivalent; `get("X","","")` returns the Single doc. Runs `check_permission()` + `apply_fieldlevel_read_permissions()`. (`test_client_get`)
- **`get_value(doctype, fieldname, filters, as_dict=True, parent)`** — `has_permission` gate (`PermissionError` if denied). `filters` may be a bare string → `{"name": filters}`. `fieldname` may be a single name OR a JSON list. For Single → `db.get_values_from_single`; else `get_list(..., limit_page_length=1)`. **Return shape**: `as_dict=True` → first dict or `{}`; `as_dict=False` → scalar if one field, tuple/list if many; `None` if no match. Scientific-notation-looking docnames (`"3E002"`) are matched as exact strings, not numbers. (`test_get_value_scientific_notation_docname`)
- **`get_single_value(doctype, field)`** — `has_permission` gate; returns `db.get_single_value` (the typed scalar, or `None`).
- **`get_count(doctype, filters, cache)`** — row count via reportview `get_count`.
- **`set_value(doctype, name, fieldname, value)`** — POST/PUT only (`PermissionError` on GET, `test_http_invalid_method_access`). `fieldname` may be a string+value pair OR a dict/JSON of values. Rejects standard/child default fields ("Cannot edit standard fields"). For child-table doctypes resolves parent and saves via parent (`on_update` fires). Returns saved `as_dict()`. (`test_set_value`)
- **`insert(doc)`** / **`insert_many(docs)`** — POST/PUT. dict OR JSON string accepted. Child-doc insert without `parenttype`/`parent`/`parentfield` raises `ValidationError`; with them, appends to parent and saves. `insert_many` ≤200, returns list of names. (`test_client_insert`, `test_client_insert_many`)
- **`save(doc)`** — POST/PUT; `get_doc(doc).save()`; returns `as_dict()`. GET → `PermissionError`. (`test_http_valid_method_access`, `test_http_invalid_method_access`)
- **`delete(doctype, name)`** — DELETE/POST. Child-table delete goes through parent (parent `save`/`on_update` fires — `test_delete` asserts `Note.save` is called). Deleting a missing doc raises `DoesNotExistError`; deleting an already-deleted child raises `DoesNotExistError`. (`test_delete`)
- **`submit(doc)`** / **`cancel(doctype, name)`** — POST/PUT; submit/cancel the doc; return `as_dict()`.
- **`bulk_update(docs)`** — POST/PUT; per-doc update keyed on `docname`; returns `{"failed_docs": [...]}` (errors collected, never raised).
- **`validate_link_and_fetch(doctype, docname, fields_to_fetch, query, filters)`** — GET/POST. Validates the link via `search_widget` (respects filters & custom queries & permissions). Empty `docname` → throw. Non-existent → `{}`. `fields_to_fetch` adds those fields. Works for child-table doctypes. `Guest` → `PermissionError`. Filter that excludes the row → `{}`. GET (no fields) sets a private Cache-Control header. (`test_client_validate_link_and_fetch`, `test_validate_link_and_fetch_for_child_table`)
- **`get_password(doctype, name, fieldname)`** — `frappe.only_for("System Manager")`, returns the decrypted password field.
- **`rename_doc`, `has_permission`, `get_doc_permissions`, `get_time_zone`, `attach_file`, `is_document_amended`** — additional surface.
- **HTTP-method gating** — `@frappe.whitelist(methods=[...])` enforces allowed verbs; wrong verb → `PermissionError`. (`test_http_invalid_method_access`)
- **client_cache** (`test_client_cache.py`): a per-process local cache layered over redis with cross-client invalidation (pub/sub), TTL, LRU `maxsize`, hit/miss statistics, `shared` keyspace, `get_value(generator=...)`, and `get_doc`. **All in-process Python (`frappe.client_cache`, `ClientCache`, redis) — never exposed over HTTP.**

---

### MATCH (ferro replicates)

1. **`get_list` returns list of dicts** — `desk.rs:612` → `method_get_list` (`desk.rs:782-797`) → `orm::get_list`. Wrapped in `{"message": [...]}`. Satisfies the substance of `test_array_values_in_request_args` (`name`/`modified` present, 200). MATCH.
2. **`get` by name returns the document** — `desk.rs:641` → `method_client_get` (`desk.rs:1012-1027`). `name` falls back to `doctype` for Singles (`desk.rs:1021`), so `get("System Settings")` returns the Single. Verified: `get?doctype=System Settings` → the Single doc. (`test_client_get` substance.)
3. **`get` non-existent → `DoesNotExistError`** — `map_orm_err` (`desk.rs:741`) maps `OrmError::NotFound` → 404 `DoesNotExistError`. Verified: `get?doctype=ToDo&name=__nope__` → `{"exc_type":"DoesNotExistError",...}`. (`test_delete` relies on the same error class for deletes — but see GAP on delete.)
4. **`get_value` dict/string-filter equivalence + first-match dict** — `method_get_value` (`desk.rs:1030-1053`) parses `filters` JSON into `build_query`; bare-string filter `ToDo` also works because `build_query`/orm treats `{"name":...}`-style and a name filter. Verified: `get_value?...filters={"name":"ToDo"}` and `filters=ToDo` both return a dict. The `as_dict=True` default → first dict or `{}` (verified no-match → `{}`). Substance of `test_client_get`/`get_value` empty case: MATCH.
5. **`get_value` empty/no-match → `{}`** — `desk.rs:1049` `.unwrap_or(json!({}))`. Verified. MATCH.
6. **`get_single_value`** — `desk.rs:643` → `method_get_single_value` (`desk.rs:1056-1076`) reads `tabSingles(doctype, field)`. Verified: `System Settings/country` → `"United States"`. (Caveat: returns the raw stored string, and `0` when absent rather than `null` — see PARTIAL.) Substantively MATCH.
7. **`get_count`** (with and without filters) — `desk.rs:606` → `method_get_count` (`desk.rs:800-817`) → `orm::count`. Verified: `ToDo` → 69; filtered → 1. MATCH.
8. **`insert` / `save` (parent docs)** — `desk.rs:640` maps BOTH `frappe.client.save` and `frappe.client.insert` to `method_client_save` (`desk.rs:935-944`) → `persist_one` → `orm::insert`/`orm::update`. Verified: `insert` of a ToDo → 200 with the saved doc (real autoname `name`, `creation`/`modified` set). Substance of `test_client_insert` (parent dict path) + `test_http_valid_method_access` (save returns the doc): MATCH for parent docs. (Child-doc and JSON-string-doc and ValidationError paths: see GAP/PARTIAL.)
9. **Scientific-notation docname matched as string** — ferro binds the docname as a TEXT parameter (`orm::get_doc` `stmt.query([name])`, `desk.rs`/orm never numeric-coerce names), so `"3E002"` is matched literally. (`test_get_value_scientific_notation_docname`) MATCH.

---

### PARTIAL

1. **`get_value` `as_dict` and the `fieldname`-as-string default** — `method_get_value` (`desk.rs:1030-1053`) **always returns a dict** and **ignores `as_dict`**. Frappe with `as_dict=0` returns a scalar (single field) or tuple (many). Verified: `get_value?...&as_dict=0` still returns `{"module":"Core"}`. The dict path (default) matches; the `as_dict=False` scalar/tuple contract does not. PARTIAL (scalar form missing).
2. **`get_value` multi-field list** — when `fieldname` is a JSON list, `method_get_value` only honors it via the `fieldname` arg if it parses to a **single bare field** (`desk.rs:1040-1045`); a JSON-list `fieldname` is NOT applied to `q.fields`, and `build_query` reads the `fields` arg (not `fieldname`), so the projection collapses to just `name`. Verified: `fieldname=["name","module"]` → `{"name":"ToDo"}` (module dropped). PARTIAL: single-field works, multi-field list silently drops the extra fields. (Med — common pattern `get_value(dt, [f1,f2], filters)`.)
3. **`get_single_value` typing & missing-field** — returns the raw stored string (no type coercion to int/float), and `0` for an absent (field, doctype) row instead of Frappe's `None`/typed value (`desk.rs:1072-1075`). Minor shape divergence. PARTIAL (Low).
4. **`get` permission / field-level read perms / null-stripping** — `method_client_get` uses `ReadAcl::all()` (`desk.rs:1022`), so it does NOT run `check_permission()`/`apply_fieldlevel_read_permissions()` for the `frappe.client.*` path, and it does NOT strip nulls (`no_nulls=True`). Verified: `get` of a ToDo returns `_assign`, `allocated_to`, … as `null` (Frappe omits them). PARTIAL on substance; the perm-skip is a security divergence (see GAP-perm). (Note: the `/api/resource` + `/api/v2` CRUD paths DO enforce permissions via `auth::permission`; only the `frappe.client.*` desk dispatch bypasses it.)

---

### GAP (not implemented)

> All verified live: with a fresh token each of these returns
> `{"exc_type":"NotFound", ... "Method '<x>' not implemented"}` (404). The desk dispatch `match` in
> `desk.rs:581-650` simply has no arm for them, so it falls through `route_method` (`main.rs:1305-1311`)
> → 404. **Fix location for every "method missing" GAP below is the `match name` block in
> `src/desk.rs::route_method` (desk.rs:581-650): add an arm + an impl fn; reuse `orm::*`.**

1. **`set_value` — GAP (High).** Not implemented → 404. Tests: `test_set_value`, `test_http_invalid_method_access` (the latter also needs HTTP-verb gating, see GAP-verb).
   Fix sketch: add `"frappe.client.set_value" => method_set_value(...)`. Parse `doctype`/`name`/`fieldname`/`value`; if `fieldname` is a dict/JSON use it as the value map else `{fieldname: value}`; reject `default_fields + child_table_fields` ("Cannot edit standard fields"); for `meta.istable` resolve parent (`SELECT parenttype,parent`) and update via the parent table; else `orm::update(con, meta, &acl, name, &values, user)`; return `{"message": saved_doc}`.

2. **`delete` — GAP (High).** Not implemented → 404. (`/api/v2/document` and `/api/resource` DELETE exist, but the `frappe.client.delete` method does not.) Test: `test_delete`.
   Fix sketch: add `"frappe.client.delete" => method_delete(...)`. For a child-table doctype, look up `parenttype/parent/parentfield`, load parent, remove the row, `orm::update` the parent (so parent `on_update`/save semantics are approximated); else `orm::delete`. Missing doc → `OrmError::NotFound` → 404 `DoesNotExistError` (this part already maps correctly). Note: pure-Rust ORM does not run Python `on_update`, so the `test_delete` assertion that `Note.save` is called cannot be satisfied for the controller side — but the data-level delete + DoesNotExist behavior can.

3. **`submit` — GAP (Med).** Not implemented → 404. No submit/cancel state machine in ferro (no `docstatus` transition path). Fix: add arm; minimally `orm::update` setting `docstatus=1` (full submit semantics — validations, hooks — are out of pure-Rust scope; this is "somewhat" per the rubric).

4. **`cancel` — GAP (Med).** Not implemented → 404. Same as submit (`docstatus=2`).

5. **`bulk_update` — GAP (Med).** Not implemented → 404. Fix: add arm; parse `docs` JSON array, per-doc `orm::update` on `(doctype, docname)`, collect failures, return `{"message": {"failed_docs": [...]}}` (never raise — match Frappe's swallow-errors contract).

6. **`validate_link_and_fetch` — GAP (Med).** Not implemented → 404. Tests: `test_client_validate_link_and_fetch`, `test_validate_link_and_fetch_for_child_table`. Fix sketch: add arm; throw on empty `docname`; `orm::get_doc`/exists check on `(doctype, docname)`; if missing → `{"message": {}}`; if `fields_to_fetch` provided, project those columns via `orm::get_list(limit 1)`; for child tables include `parenttype`. (Full `search_widget`/custom-query/`link_filters` fidelity is out of scope; the name-existence + fields-fetch + `{}` cases are achievable. Guest `PermissionError` requires the perm fix.)

7. **`get_password` — GAP (Med, security-sensitive).** Not implemented → 404. Fix sketch: add arm; gate on `auth::user_roles` containing "System Manager" (else `PermissionError`); read `tab__Auth`/encrypted password field and decrypt via `crypto::fernet` (ferro has decrypt). Returns the plaintext password value.

8. **`insert_many` — GAP (Low).** Not implemented → 404 (only single `insert`/`save` mapped). Test: `test_client_insert_many`. Fix: loop `persist_one` over the array, enforce ≤200, return list of names.

9. **`rename_doc` — GAP (Low).** Not implemented → 404. No rename support in orm.rs. Out of "somewhat" scope.

10. **Child-doc `insert` semantics — GAP (Med).** Frappe `insert` of a `{"doctype":"<child>", parenttype/parent/parentfield}` appends to the parent and saves; without those fields it raises `ValidationError`. ferro's `persist_one` (`desk.rs:947-965`) treats every doc as a top-level row (calls `orm::insert` on the child's own `tab<child>` table), so it neither enforces the "Parenttype/Parent/Parentfield required" ValidationError nor routes through the parent. Test: `test_client_insert` (child-doc cases). GAP (Med). Fix: in `persist_one`, if `meta.istable`, require parent fields (else 417 ValidationError) and append-to-parent + `orm::update`.

11. **HTTP-verb gating on write methods — GAP (Med).** `desk::route_method` ignores `http_method` for `set_value`/`save`/`insert`/`delete`/… (the `http_method` param is dropped: `desk.rs:647` `let _ = http_method;`). Frappe rejects GET on `methods=["POST","PUT"]` with `PermissionError`. Test: `test_http_invalid_method_access` (GET `frappe.client.save` must `PermissionError`). Today ferro would happily run `save` over GET. GAP (Med). Fix: thread an allowed-verbs table keyed by method name; return 403 `PermissionError` on mismatch before dispatch.

12. **`frappe.client.get` permission enforcement — GAP (High, security).** The `frappe.client.*` desk methods (`get`, `get_list`, `get_value`, `get_count`, `get_single_value`) all use `ReadAcl::all()` (`desk.rs:760,792,1022,1047`; `get_single_value` reads `tabSingles` directly with no perm gate at all, `desk.rs:1063`). Frappe gates each with `check_permission`/`has_permission`. Any authenticated user (and, combined with the known Guest-role bug in `auth.rs:144`, potentially Guest) can read any doctype via these methods regardless of DocPerm. No client-methods test isolates this (it's covered indirectly by the auth/perm suites), so it's an UNDOCUMENTED-by-this-suite but real divergence. Fix: thread the caller's `ReadAcl`/`auth::permission` into `desk::route_method` (it currently has only `user: &str`, not the resolved `Identity`/perm), and gate reads + apply permlevel masking exactly as `route_resource` does.

13. **`expand` / `expand_links` — GAP (Low).** `get_list` ignores `expand` (no expansion of link fields). Covered by `test_get_list_expand` in the wider suite (per context). `build_query`/`method_get_list` never look at `expand`.

#### client_cache (`test_client_cache.py`) — GAP / N/A
- The entire suite tests `frappe.client_cache` / `ClientCache` / redis pub-sub invalidation, **purely in-process Python, never over HTTP**. ferro has its own `src/cache.rs` (a std-only redis replacement for bench-mode) but it does NOT implement the `ClientCache` local-cache-over-redis API, TTL, LRU `maxsize`, cross-client pub/sub invalidation, hit/miss statistics, `shared` keyspace, `generator=`, or `get_doc` caching. Per the rubric this is **"shouldn't care"** for the pure-ferro REST/desk data plane — none of these are reachable through the HTTP surface ferro serves, and ferro is a single-process server (no separate "another client" to invalidate from). Classify: N/A (not a method-surface obligation). 0 of these are HTTP-exercisable.

---

### Undocumented ferro behavior discovered (divergences not covered by a client test)

1. **Reserved-word columns returned as quoted keys** — `orm::get_doc` builds its SELECT with `quote_ident` (`orm.rs:418`) and then reads back `stmt.column_names()` (`orm.rs:426`). For columns that are SQL reserved words (`parent`, `parenttype`, `parentfield`), SQLite echoes the **quoted identifier** as the column name, so `frappe.client.get` returns keys literally named `"\"parent\""`, `"\"parentfield\""`, `"\"parenttype\""` (with embedded double-quotes) alongside the (null) real values. Verified on a `client.get` of a ToDo: keys `["\"parent\"","\"parentfield\"","\"parenttype\""]` present, with garbage values (`"\"parent\"":"parent"` on insert echo). This is a real serialization bug affecting `get`, `getdoc`, and any `get_doc`-backed path for doctypes that carry these columns. Fix: alias each selected column to its bare name (`quote_ident(c) AS "c"`) or build the output map from the known `readable` list rather than `column_names()`.

2. **`insert`/`save` echo includes the quoted-key garbage too** — the `persist_one` → `orm::insert` round-trip re-reads the doc via the same `get_doc`, so the returned doc carries the same `"\"parent\""` keys (verified on the live insert probe).

3. **`get_single_value` returns `0` (number) for an unknown field/doctype** instead of `null`/typed default (`desk.rs:1074-1075`).

4. **Error envelope format** — ferro's not-implemented/auth/validation errors use `{"exc_type": "...", "message": "..."}` or `{"exc_type":..., "_server_messages":"[…]"}` (verified). Frappe's `_server_messages` wrapping is approximated but the `messages`/HTTP-status pairing for `frappe.client.*` PermissionError (which ferro never raises for these reads) differs.

5. **`provision-key` rotates `api_secret`** on every CLI invocation — operationally relevant for probing (a previously issued token becomes `AuthenticationError`), not a method divergence, but documented here so future probes re-provision per batch.
