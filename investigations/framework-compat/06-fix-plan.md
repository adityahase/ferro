# 06 — Fix plan (confirmed gaps), prioritized

Scope rule (from the goal): *make as few changes as possible, don't reimplement unnecessary parts,
keep the memory footprint low.* Each fix below notes whether it's a must-fix correctness item or a
scoped decision. Line numbers are against the working tree at audit time.

Priority legend: **P0** correctness/security must-fix · **P1** real compatibility gap · **P2**
decision (scope in/out) · **P3** cosmetic/low.

---

## FIX-1 (P0, security) — Role resolution: stop granting "All" to Guest
**Gap:** G1/D1. `auth.rs:143 user_roles()` seeds `vec!["All"]` for *every* user, so Guest inherits
the `All` role and can read any doctype whose DocPerm grants `All` (ToDo, Note, …). Logged-in users
also wrongly miss the implicit `Guest` (and `Desk User`) roles.

**Frappe spec** (`frappe/permissions.py get_roles`):
- `Guest` → `["Guest"]` only.
- `Administrator` → all roles (already special-cased elsewhere in ferro).
- normal user → `<Has Role rows, excluding AUTOMATIC_ROLES>` + `["All","Guest"]` + `["Desk User"]`
  iff `User.user_type == "System User"`.
- Constants: `All`, `Guest`, `Desk User`, `Administrator` are the AUTOMATIC_ROLES.

**Current code (`src/auth.rs:143`):**
```rust
fn user_roles(con: &Connection, user: &str) -> Vec<String> {
    let mut roles: Vec<String> = vec!["All".to_string()];
    if user == "Guest" {
        roles.push("Guest".to_string());
    }
    if let Ok(mut stmt) =
        con.prepare("SELECT role FROM \"tabHas Role\" WHERE parent=?1 AND parenttype='User'")
    { /* push rows */ }
    roles
}
```
**Replacement:**
```rust
fn user_roles(con: &Connection, user: &str) -> Vec<String> {
    // Guest never gets the "All" role (Frappe: get_roles("Guest") == ["Guest"]).
    if user == "Guest" {
        return vec!["Guest".to_string()];
    }
    let mut roles: Vec<String> = Vec::new();
    if let Ok(mut stmt) = con.prepare(
        "SELECT role FROM \"tabHas Role\" \
         WHERE parent=?1 AND parenttype='User' \
           AND role NOT IN ('All','Guest','Desk User','Administrator')",
    ) {
        if let Ok(rows) = stmt.query_map([user], |r| r.get::<_, String>(0)) {
            roles.extend(rows.flatten());
        }
    }
    roles.push("All".to_string());
    roles.push("Guest".to_string());
    // "Desk User" only for System Users (relevant for desk-only doctype perms).
    let is_system: bool = con
        .query_row(
            "SELECT 1 FROM \"tabUser\" WHERE name=?1 AND user_type='System User'",
            [user], |_| Ok(true),
        )
        .unwrap_or(false);
    if is_system {
        roles.push("Desk User".to_string());
    }
    roles
}
```
**Blast radius:** `permission()` and `readable_permlevels()` both call this; both already short-circuit
Administrator, so the change only affects non-admins. Footprint: +1 cheap query per non-cached perm
check (acceptable; or memoize per request).
**Regression lock:** add a Rust unit test + a `verify.py` assertion: Guest `GET /api/resource/ToDo`
→ 403; Guest `GET /api/resource/ToDo/<name>` → 403; an authenticated System-User reading a
`Desk User`-only doctype → 200.

---

## FIX-2 (P2, decision) — Sessions & login: make `login` honest
**Gap:** G2/D2/D3. `login` (`desk.rs:583`) returns 200 unconditionally — **no credential check** —
and ferro has no session store, so the issued `sid` is never validated; cookie clients fall back to
`default_user`.

Two coherent options — pick one and document it:

### Option A (recommended, minimal): make login + sid real, backed by `tabSessions`
Frappe already persists sessions in `tabSessions(sid, user, sessiondata, lastupdate, ...)`. ferro can
read/write that table — no Redis, low footprint.
1. **Verify credentials in `login`** (`desk.rs:583`): resolve `usr` honoring System Settings
   (`allow_login_using_user_name`/`allow_login_using_mobile_number` → match `tabUser.username`/
   `mobile_no`); reject if `disable_user_pass_login`; verify password with the existing
   `crypto::pbkdf2` path against `__Auth(fieldname='password')`. On failure return the Frappe shape
   (`401`/`{"message":"Invalid login credentials"}`) so `FrappeClient` raises `AuthError`.
2. **Mint a session**: INSERT a row into `tabSessions` (sid=random, user, expiry from
   `System Settings.session_expiry`), set `sid` cookie with `Expires`/`Max-Age` in
   `main.rs:858 attach_login_cookies()` (currently uses `default_user`; pass the real user + expiry).
3. **Validate sid** in `auth.rs resolve_user` (`auth.rs:37`): before falling to `default_user`, if a
   `sid` cookie is present, look it up in `tabSessions`, check expiry, and resolve that user. Add a
   small `Cookie` parse in `main.rs route()` (currently only the `Authorization` header is read).
4. (Optional) `deny_multiple_sessions`: delete other `tabSessions` rows for the user on login.
**Footprint:** a few SQL statements; no new deps. This unlocks `test_auth` (policy + cookie-expiry),
FrappeClient, and real per-user Desk.

### Option B (smallest): scope sessions out, make the stub safe
If sessions are deliberately out of scope: **remove** the cosmetic `login` 200-stub and the
`sid`/`user_id` cookies (`main.rs:858`), returning `501`/`417` from `login` so no client mistakes it
for real auth, and document "ferro is token-auth only." Keep `--desk`'s `default_user=Administrator`
behind an explicit opt-in (see FIX-7). This is the low-effort path but breaks cookie clients by
design.

**Decision driver:** do you need non-admin Desk / FrappeClient / browser login? If yes → A. If ferro
is purely a token API + single-tenant Desk → B.

---

## FIX-3 (P1) — `expand` / `expand_links` in list & get
**Gap:** G3. List/get return the raw link value; Frappe replaces it with the linked doc dict when
`expand`/`expand_links` is passed.
- **Parse** params in `main.rs:1434 build_list_query()` (add `expand: Vec<String>`) and in
  `route_resource` GET-single (read `expand_links`).
- **Apply** in `orm.rs:288 get_list()` / `orm.rs:401 get_doc()`: for each requested link field with a
  non-empty value, look up the link target's meta (`meta.rs`) and fetch the target doc (respecting
  the caller's read ACL) and substitute the dict. `expand_links=true` expands *all* Link fields;
  `expand=[...]` expands the named fields.
**Footprint:** one extra query per expanded link; gate strictly on the param so the default path is
unchanged. **Lock:** `test_get_list_expand` / `test_get_doc_expand` shapes.

---

## FIX-4 (P1) — v2 bulk operations
**Gap:** G4. `/api/v2/document/<dt>/bulk_delete` → 405; `/api/v2/method/bulk_delete` → 404;
`bulk_delete` invalid format should be **417** `{"errors":[{"exception":"'docs' must be a list"}]}`.
- Implement in `main.rs:1205 route_v2_method` (cross-doctype `bulk_delete`/`bulk_update`, taking
  `docs:[{doctype,name}]`) and in `route_v2_document` (per-doctype `bulk_delete` taking `names:[...]`).
- Response shape Frappe returns: `{"data":{"total":N,"success_count":S,"failure_count":F,
  "deleted":[...],"failed":[{"name":..,"exception":..}]}}`. Validate `docs`/`names` is a list → else
  417 with `errors[]`. Reuse `orm::delete` per item, catching per-item errors into `failed`.
- `bulk_update`'s *enqueue* path needs background jobs; the synchronous path can reuse `orm::update`.
**Lock:** `test_bulk_delete_docs_single_doctype/_partial_failure/_cross_doctype/_invalid_format`.

---

## FIX-5 (P3→P1 case-by-case) — Missing whitelisted methods
**Gap:** G5.
- `frappe.realtime.get_user_info` → add to `desk::route_method` (or `ferro_method`) returning
  `{"message": {}}` (server-to-server default). One line. **Lock:** `test_get_user_info`.
- `frappe.core.doctype.data_import...download_template` and any other app/Python method: **out of
  scope for pure ferro** — these are the `ferrod` Python-fallthrough tier's job. Document, don't port.
- General: enumerate the `@frappe.whitelist()` methods that the frappe-ui frontends actually call and
  port the small, pure-data ones; leave the rest to `ferrod` (see `07-…` for the capture approach).

---

## FIX-6 (P3) — `debug=1` → `_debug_messages`
**Gap:** G6. When `debug` is truthy, Frappe adds a `_debug_messages` JSON-string array to the
response. Add in `main.rs route_resource`/`route` success path: if `params.get("debug")` is truthy,
inject `"_debug_messages": "[]"` (or collected SQL). Cosmetic/dev-only; lowest priority.
**Lock:** `test_get_list_debug`.

---

## FIX-7 (P0 guardrail) — `--desk` silently sets default_user=Administrator
**Gap:** D5. `main.rs:601` flips `default_user` to `Administrator` when `--desk` is set without
`--default-user`, so `ferro serve --desk` authenticates *every* unauthenticated request as admin.
**Fix:** require an explicit `--default-user Administrator` (or a `--insecure-desk-admin` flag) to
enable that posture; otherwise default to `Guest` even in desk mode, and log a clear warning when
admin-default is active. Prevents an accidental "auth-off" internet deployment.

---

## FIX-9 (P0, security) — `frappe.client.*` read methods bypass permissions
**Gap:** D8/GAP-12. The desk method path reads with `ReadAcl::all()` and no `auth::permission`, so
`/api/method/frappe.client.get_list|get|get_value|get_count|get_single_value` leak any doctype to any
user (Guest reads User emails — confirmed). Active whenever `--desk` is on (Desk/SPA/signup).

**Root cause:** `desk::route_method` (`desk.rs:569`) takes only `user: &str`; the client-read arms
build `ReadAcl::all()` (`desk.rs:760,792,1022,1047`) and `get_single_value` reads `tabSingles` ungated
(`desk.rs:1063`). Compare `route_resource` (`main.rs:1339`) which does
`auth::permission(con,&meta,user,"read")` → 403, then `ReadAcl{permlevels: readable_permlevels(...)}`.

**Fix:**
1. Thread the resolved identity/permission into the desk dispatch: change `route_method(..., user: &str, ...)`
   to take the `auth::Identity` (or at least call `auth::permission` + `readable_permlevels` inside each
   client-read arm), exactly as `route_resource` does.
2. In each `frappe.client.{get,get_list,get_value,get_count,get_single_value}` arm: resolve `meta`,
   `let perm = auth::permission(con,&meta,user,"read"); if !perm.allowed { return PermissionError(403) }`,
   build `ReadAcl{ permlevels: auth::readable_permlevels(con,&meta,user) }` instead of `ReadAcl::all()`,
   and apply `if_owner` scoping (`perm.only_if_owner`) like the resource path.
3. `get_single_value` must also gate on read permission of the Single doctype.
**Lock:** Guest `GET /api/method/frappe.client.get_list?doctype=User` → 403; a permlevel-0 user reading
`User` via `frappe.client.get` gets no `api_key`/`api_secret` (permlevel masking), matching `/api/resource`.
**Note:** this fix is independent of FIX-1 (Guest-`All`) and FIX-2 (sessions); all three are needed for a
correct auth posture. Pair with FIX-7 so `--desk` doesn't also default everyone to Administrator.

## FIX-8 (P1) — Corrupt parent/parentfield/parenttype keys on non-child doctypes
**Gap:** D7/G7. `meta.columns` includes child-only columns for every doctype, so `get_doc` selects
`"parent"` from tables that lack it → SQLite returns the quoted-identifier as a string literal →
corrupt JSON keys `"\"parent\"":"parent"` on ~146 doctypes (User, DocType, Role, …).

**Root cause (`src/meta.rs`):**
```rust
// meta.rs:12 — parent/parentfield/parenttype are NOT on every table (child tables only)
pub const STANDARD_COLUMNS: &[&str] = &[
    "name","creation","modified","modified_by","owner","docstatus","idx",
    "parent","parentfield","parenttype","_user_tags","_comments","_assign","_liked_by",
];
// meta.rs:156 — seeds columns with ALL standard cols, THEN adds PRAGMA (real) cols
let mut columns: HashSet<String> = STANDARD_COLUMNS.iter().map(|s| s.to_string()).collect();
```
**Fix (preferred):** for physical tables, make PRAGMA authoritative — don't pre-seed:
```rust
let mut columns: HashSet<String> = HashSet::new();
if !issingle && !is_virtual {
    // PRAGMA is the source of truth for an existing table (standard + docfield + custom).
    if let Ok(mut stmt) = con.prepare(&format!("PRAGMA table_info({})", quote_ident(&table))) {
        if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(1)) {
            for c in rows.flatten() { columns.insert(c); }
        }
    }
} else {
    // singles / virtual: synthesize from docfields + the columns a single actually has
    columns.extend(SINGLE_STANDARD_COLUMNS.iter().map(|s| s.to_string())); // name/owner/... no parent*
    for f in &fields { if !f.is_virtual_column() { columns.insert(f.fieldname.clone()); } }
}
```
**Alternative (smaller):** drop `parent`/`parentfield`/`parenttype` from `STANDARD_COLUMNS` and add
them back only when `istable`. Either way, the get_doc SELECT then only names real columns.
**Belt-and-suspenders:** SQLite `PRAGMA legacy_double_quoted_strings = OFF` would turn the misfeature
into an error instead of silent corruption — worth setting on every ferro connection regardless.
**Lock:** GET `/api/resource/User/Administrator` must NOT contain any `parent*` key; child-doc GET
still includes real `parent`/`parentfield`/`parenttype`.

---

# Behavioral-domain fixes (appendix)
Consolidated from the deep-dives in `_raw/behavior_*.md` (each has the full before/after code sketch
and the spec-test cite). Grouped by ferro module; severities carried from `02`/`04`.

## naming (`src/naming.rs`)
- **B-NAM-1 (High) ISO `WW` week** — `naming.rs:197` replace `(day_of_year+6)/7` with a real
  `determine_consecutive_week_number` (ISO `%V` + Jan/Dec boundary fixups → "00"/"53"); add
  `util::iso_week(y,m,d)`. Diverges *today* and corrupts `tabSeries` keys vs a CPython worker.
- **B-NAM-2 (High) child-table autoname** — `orm.rs:711 insert_children` calls `random_name()` for
  every child; instead load the child `Meta` and call `naming::resolve_name`, and keep an existing
  child `name` on update (no rename-on-edit).
- **B-NAM-3 (Med)** `revert_series_if_last`, **B-NAM-4 (Med)** `append_number_if_name_exists`,
  **B-NAM-5 (Med)** amended/cancelled naming — add the three functions (naming.py:441/append/`_set_amended_name`
  are the references); wire append into the duplicate path so `Bottle`→`Bottle-1` instead of 409 where Frappe appends.
- **B-NAM-6 (Low)** required-field message → use `meta.get_label(fieldname)` not the raw fieldname
  (`naming.rs:57`); validate UUID/int `name` (`naming.rs:34`); microsecond timestamp in `util::now_civil`.

## ORM filters (`src/orm.rs`)
- **B-FIL-1 (Med)** accept dict elements inside a filter array (`orm.rs:222` Array arm) — for
  `or_filters=[{...},{...}]` and dict-in-list `filters`.
- **B-FIL-2 (Med)** `in/not in None`: `in null`→no rows, `not in null`→all rows; strip null elements
  (`orm.rs:171`/`in_values orm.rs:143`).
- **B-FIL-3 (Med)** `in/not in` value that is a JSON-encoded list string: `serde_json::from_str` first,
  fall back to comma-split (`orm.rs:143`) — preserves embedded commas.

## ORM document (`src/orm.rs`, `src/meta.rs`)
- **B-DOC-1 (Med) link validation** — after required-field loop (`orm.rs:651`) + in `insert_children`
  + `update`: for non-empty Link/Dynamic-Link values, `SELECT 1 FROM "tab<options>" WHERE name=?`;
  none → `LinkValidationError` (417). Needs Link `options` on `DocField` (`meta.rs:17`).
- **B-DOC-2 (Med) `set_only_once`** — parse `set_only_once` into `DocField` (`meta.rs`), and in
  `update` (`orm.rs:800`) reject a changed value with `CannotChangeConstantError` (417).
- **B-DOC-3 (Med) optimistic lock** — in `update`, if payload has `modified`, compare to DB; mismatch
  → `TimestampMismatchError` (`orm.rs:782`).
- **B-DOC-4 (Med, scoped) submit/cancel/docstatus** — out of pure-ferro scope unless needed; document.

## db-api / client methods (`src/desk.rs`)
- **B-DB-1 (High) Single doctypes** — `frappe.client.get_value`/`get_list` on a Single must read
  `tabSingles` (key/value), not `tab<Single>` (which doesn't exist). Reuse ferro's `get_single`.
- **B-DB-2 (Med)** `get_value` multi-field (`fieldname` as JSON list) → return all requested fields;
  string `filters` → coerce to `{"name": <s>}`. `get_single_value` → cast by fieldtype, None not `0`.
- **B-DB-3 (Low)** aggregates (`{"COUNT":f}`), `distinct`/`group_by`/`pluck`/`as_list`, field alias.

## meta (`src/meta.rs`)
- **B-MET-1 (Med) Custom Fields** — merge `tabCustom Field` rows into `meta.fields` (Frappe does at
  `get_meta`); otherwise custom fields are invisible to ORM/get_doc.
- **B-MET-2 (Med) Property Setters** — apply `tabProperty Setter` overrides (field props, perms,
  options) so per-site customizations take effect.
- **B-MET-3 (Med) cache invalidation** — add a way to drop a doctype from `MetaCache` on DocType
  change (or short TTL) so runtime schema edits are seen. High if developer/runtime DocType edits matter.

## REST envelopes (`src/main.rs`)
- **B-REST-1 (Med) v2 auth-error envelope** — `main.rs:987` resolves auth and builds the 401 with `err()`
  (V1 shape `{exc_type,_server_messages}`) *before* the version branch, so `/api/v2/*` 401s are V1-shaped.
  Move auth resolution below the version split (or pass an `is_v2` flag) and emit `err_v2()` for v2 paths.
- **B-REST-2 (Med) v2 `doctype` subpath** — `route()` v2 match only handles `document`/`method`
  (`main.rs:1033`); add a `Some("doctype")` arm: `/api/v2/doctype/<dt>/meta` → serialized meta,
  `/count` → `{"data": <int>}`, both read-perm-gated, `err_v2` on miss. (TestDocTypeAPIV2.)
- **B-REST-3 (Low) v2 `copy`** — `/api/v2/document/<dt>/<name>/copy` is swallowed into the doc name;
  add a `copy` arm returning the doc minus `name/owner/creation/modified/modified_by/docstatus`.
- **B-REST-4 (Low) read-only mode** — honor `maintenance_mode` from site_config: writes → 503
  `InReadOnlyMode` (`err`/`err_v2`). (TestReadOnlyMode v1+v2.)

## client methods (`src/desk.rs`)
- **B-CLI-1 (Med) `client.get` null-stripping** — `frappe.client.get` should return
  `doc.as_dict(no_nulls=True)` (client.py:117) — ferro returns all fields incl. nulls. Strip
  null-valued keys in the `frappe.client.get` arm. (Also relevant to response-size/footprint.)
- See `_raw/behavior_client-methods.md` for the full method-by-method parity table.

---

## Suggested landing order
1. **Security first:** FIX-9 (`frappe.client.*` perm bypass) + FIX-1 (Guest-`All` role) + FIX-7 (desk
   admin-default guardrail). These three together fix the auth posture; all small, no scope questions.
2. Correctness bugs: FIX-8 (parent-key corruption) + B-DB-1 (Single doctypes) + B-NAM-1/2 (WW week,
   child autoname) + B-FIL-1/2/3 (filter shapes). Localized, high-value.
3. FIX-5 (get_user_info) + FIX-6 (debug) + B-REST-1/2 (v2 envelope + doctype subpath) — small stubs/arms.
4. FIX-3 (expand) + FIX-4 (v2 bulk) + missing `frappe.client.*` write methods — features frappe-ui uses.
5. FIX-2 (sessions/login) — the one architectural decision; do Option A or B deliberately.
6. Remaining behavioral-domain fixes (link validation, set_only_once, optimistic lock, Custom Fields/
   Property Setters in meta, db-api casting, null-stripping) from the appendix above.
