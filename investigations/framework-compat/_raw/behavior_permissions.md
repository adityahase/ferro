# Behavioral fidelity: permissions

Spec sources (under `/home/frappe/benches/bench-cpython314/apps/frappe/`):
- `frappe/tests/test_permissions.py` (TestPermissions — 40 tests; the canonical perm spec)
- `frappe/permissions.py` (the implementation the tests encode: `get_roles`, `has_permission`,
  `get_role_permissions`, `get_doc_permissions`, `has_user_permission`)
- `frappe/core/doctype/user_permission/test_user_permission.py` (User Permissions)
- `frappe/core/doctype/docshare/test_docshare.py` (DocShare)

ferro source compared: `/home/frappe/ferro/src/auth.rs` (`user_roles`, `permission`,
`readable_permlevels`, `owns`, `ptype_for_method`), and the integration in
`/home/frappe/ferro/src/main.rs` (`route_v2_document` / `route_resource`) + `/home/frappe/ferro/src/orm.rs`
(`ReadAcl`, `get_list` owner_scope, `doc_owner`).

Judgment rubric for this domain: **1:1** — ferro explicitly reimplements the DocPerm role/permlevel/if_owner
contract for its REST data plane and MUST match the substantive assertions. User Permissions / DocShare /
controller `has_permission` hooks are subsystems ferro intentionally omits → those tests are *somewhat /
shouldn't-care* for pure ferro, but the omission is a real security/visibility divergence and is recorded
as a GAP because ferro's perm model is a *superset-of-access* relative to Frappe (it grants more than Frappe).

All probes below ran against the live ferro server `http://127.0.0.1:8081` (site `bench-test/mysite.sqlite`),
provisioning tokens with `ferro provision-key`.

---

## permissions

### Behaviors Frappe guarantees (bullet list)

Role resolution (`get_roles`, permissions.py:535):
- B1. `Guest` (or empty user) → exactly `["Guest"]`, nothing else (permissions.py:540-541; `test_automatic_permissions`).
- B2. A logged-in **Website User** → their `Has Role` rows (minus AUTOMATIC_ROLES) **+ `["All","Guest"]`** (permissions.py:558; `test_automatic_permissions` asserts `GUEST_ROLE, ALL_USER_ROLE`).
- B3. A logged-in **System User** → also gets `"Desk User"` (SYSTEM_USER_ROLE) (permissions.py:559-560; `test_automatic_permissions`).
- B4. `Administrator` → **all** roles in the system, i.e. allowed everything (permissions.py:544-545, has_permission:107-109; every test sets Administrator to bypass).
- B5. AUTOMATIC_ROLES = `("Guest","All","Desk User","Administrator")` (permissions.py:33-40).

Role→DocPerm matching (`get_role_permissions`, permissions.py:282):
- B6. Only permlevel-0 DocPerm rows whose `role` is in the user's roles contribute to doctype-level rights (`is_perm_applicable`, permissions.py:314-315). A right is granted if ANY applicable perm sets it (permissions.py:325).
- B7. `select` is implied by `read` (has_permission:212-222; `test_select_permission`: a Sales User with only `select` still passes `has_permission("select")` but fails `read`/`write`).
- B8. **Custom DocPerm completely overrides standard DocPerm** when present (`test_overrides_work_as_expected`). The 4 meta doctypes `DocType/DocField/DocPerm/Custom DocPerm` are never overridable.

if_owner scoping (`get_role_permissions`:328-338, `get_doc_permissions`:255-273):
- B9. When some-but-not-all applicable perms for a ptype have `if_owner=1` (and no non-if_owner perm grants it), the right becomes owner-conditional: the user keeps `select`/`read` at the doctype level (so list works) but `write`/`delete`/etc. collapse to 0 unless they own the row (permissions.py:334-338).
- B10. `create` is **never** owner-scoped (permissions.py:332 `ptype != "create"`; get_doc_permissions:268-269 forces `if_owner.create=0`). `test_insert_if_owner_with_user_permissions` asserts the owner can read+write but **not** create.
- B11. List queries under if_owner add an `owner = <user>` filter so the owner sees only their rows (`test_if_owner_permission_on_get_list`); a non-owner's `get_list` returns `[]` not an error (`test_if_owner_permission_overrides_properly`:495-496).
- B12. Single-doc `has_permission`/`getdoc` for a non-owned doc under if_owner raises PermissionError (`test_if_owner_permission_on_getdoc`:564-565).
- B13. Owner equality is case-insensitive lowercase compare (`is_user_owner`, permissions.py:235).

permlevel field masking:
- B14. Reads are filtered to fields whose docfield `permlevel` the user has a `read` grant for; permlevel-0 fields are always returned when read is granted at all. `test_select_permission`:101-104 asserts a select-only user's `get_list(fields="*")` returns a **subset** (default_fields + search fields), not the full record.

User Permissions (apply_user_permissions) — `test_user_permissions_in_doc/report`, etc.:
- B15. A `User Permission` linking user→(linkdoctype,value) restricts visibility: docs whose link field points outside the allowed values are hidden in lists and 403 on `has_permission` (`test_user_permissions_in_report`, `test_user_permission_doctypes`).
- B16. If the user owns a doc but fails User Permissions, access collapses to `if_owner` perms only, with `create=0` (get_doc_permissions:264-269).
- B17. User Permissions are *not* applied if the user's role doesn't grant the right in the first place (`test_user_permission_is_not_applied_if_user_roles_does_not_have_permission`).
- B18. `apply_strict_user_permissions` System Setting toggles whether docs with no matching User Permission for a linked doctype are shown (`test_strict_user_permissions`).
- B19. User Permissions can seed default link-field values on `new_doc` (`test_default_values`, test_user_permission.py `test_default_user_permission`).

DocShare — `test_docshare.py`:
- B20. `frappe.share.add(dt, name, user[, write, share])` grants per-document access; a shared doc passes `has_permission` even with no role perm (`test_doc_permission`, `test_list_permission`).
- B21. share with `share=1` cascades read+write; `everyone=1` shares to all users (`test_share_permission`, `test_share_with_everyone`).

Controller / hook permissions:
- B22. `has_controller_permissions` (the app `has_permission` hook) can veto access before role perms even apply (get_doc_permissions:237-239).

Standard-field & constant protections (in this test file, ORM-adjacent):
- B23. `owner` and `creation` cannot be set/changed by a client on insert or save (`test_set_standard_fields_manually`, `test_dont_change_standard_constants`).

---

### MATCH (ferro replicates)

- **B4 Administrator bypass** — `auth.rs:182-188` `permission()` returns `allowed:true, only_if_owner:false` for Administrator; `auth.rs:229` `readable_permlevels` returns `None` (read all permlevels); `auth.rs:144`/`user_roles` is bypassed entirely. Probe: admin GET `/api/v2/document/User/Administrator` returned all fields incl permlevel-1 (`api_key`, `roles`-area). (`test_automatic_permissions` admin branch; `test_basic_permission`.)

- **B6 permlevel-0 role→DocPerm matching** — `auth.rs:197-204` counts grants `WHERE parent=? AND COALESCE(permlevel,0)=0 AND COALESCE(<col>,0)=1 AND role IN (...)`, exactly Frappe's `is_perm_applicable` (permlevel==0, role∈roles, any perm grants). Probe: Guest→User list = 403 (User grants only System Manager/Desk User at pl0, not All). (`test_get_doctypes_with_read`, `test_overrides_work_as_expected` partial.)

- **B8 Custom DocPerm override** — `auth.rs:163-179` `perm_table()` selects `tabCustom DocPerm` when any row exists for the doctype, else `tabDocPerm`, and excludes the 4 meta doctypes + istables (`auth.rs:138-140`). This is "custom completely overrides standard" + the meta-doctype exclusion. (`test_overrides_work_as_expected`.)

- **B9/B11 if_owner list scoping** — `auth.rs:215-219` sets `only_if_owner = uncond==0 && owner_only>0 && col!="create"`; `main.rs:1130` passes `owner_scope = Some(user)` into `orm::get_list`, and `orm.rs:327-330` ANDs `owner = ?`. Probe: test1 (only the `All` if_owner read on Contact) listed `FERRO_OWN%` → got ONLY `FERRO_OWN_TEST1`, not admin's row. (`test_if_owner_permission_on_get_list`.)

- **B11 non-owner list returns empty (not error)** — because the doctype-level `permission().allowed` is true (owner_only>0) the list query runs and just filters to zero rows; ferro returns `{"data":[]}` not 403. Matches `test_if_owner_permission_overrides_properly`:495-496.

- **B12 single-doc owner pre-check** — `main.rs:1102-1110` `owner_violation()` + the `("GET", Some(n))` branch (main.rs:1143-1146) loads `doc_owner` (orm.rs:952) and 403s when `only_if_owner` and the user isn't the owner. Probe: test1 GET own contact = 200, GET admin's contact = 403. (`test_if_owner_permission_on_getdoc`.)

- **B10 create never owner-scoped** — `auth.rs:218` `col != "create"` forces `only_if_owner=false` for create; `permission().allowed` is still true from the `owner_only>0` count. Probe: test1 POST `/api/v2/document/Contact` (Contact grants `All` create=1,if_owner=1) → **200**, new doc owned by test1. (`test_insert_if_owner_with_user_permissions`:355,365.)

- **B13 case-insensitive owner equality** — `auth.rs:223-225` `owns()` lowercases both sides, matching `is_user_owner` permissions.py:235.

- **B14 permlevel field masking** — `orm.rs:28-45` `ReadAcl.can_read` keeps only fields whose docfield permlevel ∈ `readable_permlevels`; `auth.rs:228-255` collects the distinct permlevels the user's roles have `read` on (+ always permlevel 0). Wired at main.rs:1099-1101. Probe: System Manager (has User pl0 **and** pl1 read) GET a User → permlevel-1 fields (`role_profile_name`, `block_modules`) present, as Frappe would. The select-only subset assertion (`test_select_permission`:101-104) is satisfied in spirit — a user lacking higher permlevels gets pl0 fields only. **(see PARTIAL on the select-fields exactness.)**

- **B5 AUTOMATIC_ROLES recognized for Custom DocPerm exclusion** — the meta-doctype exclusion list (`auth.rs:138-140`) matches Frappe's `set_custom_permissions` exclusion. (No direct test; encoded by `test_overrides_work_as_expected`.)

### PARTIAL

- **B2/B3 logged-in role resolution** — `auth.rs:143-159` `user_roles()` returns `["All"] + (HasRole rows)` for every non-Guest user, and adds `"Guest"` **only** for the Guest user. So:
  - It correctly gives a logged-in user `"All"` + their explicit roles (the part that drives nearly all DocPerm matches) — this is why most reads behave correctly.
  - **But it never adds `"Guest"` to logged-in users** (Frappe does, permissions.py:558) and **never adds `"Desk User"` (SYSTEM_USER_ROLE)** to system users (permissions.py:559-560). A doctype whose only grant is to role `"Guest"` or `"Desk User"` would be invisible to a logged-in/system user under ferro though Frappe grants it. `test_automatic_permissions` asserts a website user has `GUEST_ROLE, ALL_USER_ROLE` and a system user additionally `SYSTEM_USER_ROLE` — ferro fails both of those specific membership assertions. Substantive doctype access is *mostly* right because "All" carries most grants; this is PARTIAL, not full GAP. Severity Med (some Desk-User-gated doctypes wrongly 403 for system users).

- **B7 select implied by read** — ferro maps HTTP method→ptype via `auth.rs:119-127` `ptype_for_method` (GET→read, POST→create, …). There is **no `select` ptype over the REST surface** and no "select implies read" cascade. Reads are gated on the `read` column only (`auth.rs:190`). For the data plane this is acceptable (a select-only user has no `read` and is 403'd, where Frappe also 403s `read`); but ferro cannot express the select-only "can list-but-not-read-full" state. Partial: the REST contract (read==403 for select-only) holds; the nuanced select capability does not exist.

- **B14 select-only field subset exactness** — ferro masks by **permlevel** but not by the search-field/default-field subset that Frappe returns for a *select-only* perm (`test_select_permission`:104 `assertSequenceSubset(default_fields + get_search_fields(), permitted_record)`). ferro has no notion of "select grants only the search fields"; with no read grant it returns 403 instead of a trimmed record. Permlevel masking is exact; select-subset masking is absent.

### GAP

- **G1 — Guest is granted role "All" (security)** — **Severity: HIGH.**
  `auth.rs:144` seeds `let mut roles = vec!["All".to_string()];` for **every** user including Guest, and only *adds* "Guest" on top (`auth.rs:147-149`). Frappe returns `["Guest"]` for Guest with **no "All"** (permissions.py:540-541). Effect: an unauthenticated request is resolved as Guest (default_user) and inherits every `All`-role DocPerm grant.
  **Probe (confirmed):** unauthenticated GET `/api/v2/document/ToDo` → **200** returning rows (`ToDo` grants `All`/read at pl0 — verified in DB). Frappe would 403 (Guest has no ToDo grant). This is the same bug noted as #1 in the shared empirical findings and it breaks `test_automatic_permissions` Guest branch (Guest must be `[GUEST_ROLE]` only).
  **Fix location:** `auth.rs:142-160` `user_roles()`.
  **Fix sketch:**
  ```rust
  fn user_roles(con: &Connection, user: &str) -> Vec<String> {
      // Guest (or empty) → exactly ["Guest"], matching frappe.get_roles.
      if user == "Guest" || user.is_empty() {
          return vec!["Guest".to_string()];
      }
      // Logged-in users: explicit Has Role rows + ["All","Guest"] (+ "Desk User" if System User).
      let mut roles: Vec<String> = Vec::new();
      if let Ok(mut stmt) = con.prepare(
          "SELECT role FROM \"tabHas Role\" WHERE parent=?1 AND parenttype='User'") {
          if let Ok(rows) = stmt.query_map([user], |r| r.get::<_,String>(0)) {
              for r in rows.flatten() { roles.push(r); }
          }
      }
      roles.push("All".to_string());
      roles.push("Guest".to_string());
      // System User → add "Desk User" (fixes G2). user_type='System User' on tabUser.
      let is_system: bool = con.query_row(
          "SELECT user_type FROM \"tabUser\" WHERE name=?1", [user],
          |r| r.get::<_,String>(0)).map(|t| t == "System User").unwrap_or(false);
      if is_system { roles.push("Desk User".to_string()); }
      roles
  }
  ```
  (Administrator never reaches here — `permission()`/`readable_permlevels` short-circuit it.)

- **G2 — "Desk User" (SYSTEM_USER_ROLE) never granted to system users** — **Severity: Med.** Folded into the G1 fix above (the `is_system` branch). Without it, a System User cannot access any doctype whose only grant is to role `Desk User`. (`test_automatic_permissions` system-user branch.) Fix location: same `auth.rs:user_roles`.

- **G3 — "Guest" role never granted to logged-in users** — **Severity: Low.** Also folded into G1 (`roles.push("Guest")`). Without it, a logged-in user can't see a doctype whose only grant is to role `Guest` (rare; most such doctypes also grant All). (`test_automatic_permissions`.)

- **G4 — User Permissions (apply_user_permissions) not implemented** — **Severity: Med (visibility/over-grant).**
  ferro has zero references to `tabUser Permission` (`grep` in auth.rs/orm.rs/main.rs = none). Frappe restricts both list visibility and single-doc access by link-field value (B15-B19). Effect: a user that Frappe would scope down by a User Permission sees **more** rows in ferro than Frappe allows — ferro's access is a superset. Breaks `test_user_permissions_in_doc/report`, `test_user_permission_doctypes`, `test_select_user`, `test_contextual_user_permission`, `test_strict_user_permissions`, and all of `test_user_permission.py`.
  **Fix location:** new logic in `orm.rs::get_list` (add link-field `IN (...)` filters) + a per-doc check in `main.rs::route_v2_document`/`route_resource`, driven by a new `auth.rs` helper reading `tabUser Permission` (filter on `user`, group by `allow` doctype → set of allowed `for_value`s, honoring `applicable_for`/`is_default`/`apply_to_all`). Also wire `apply_strict_user_permissions` from System Settings.
  **Fix sketch:** load `SELECT allow, for_value, applicable_for FROM "tabUser Permission" WHERE user=?`; for each link docfield whose `options` is a restricted doctype, AND `"<linkfield>" IN (allowed_values)` into the list WHERE; for single-doc GET, 403 if the doc's link value ∉ allowed (unless owner → collapse to if_owner per B16). Large feature; matches the project's "intentionally absent subsystem" note, so flagged Med not High — but it is the largest correctness gap in the domain.

- **G5 — DocShare not implemented** — **Severity: Med (under-grant).**
  No `tabDocShare`/`frappe.share` logic anywhere in ferro. A document shared with a user via DocShare passes Frappe's `has_permission` even with no role perm (B20-B21); ferro returns 403. Effect: ferro is *more* restrictive than Frappe here (opposite direction from G1/G4). Breaks all of `test_docshare.py` (`test_doc_permission`, `test_list_permission`, `test_share_permission`, `test_share_with_everyone`, …).
  **Fix location:** `auth.rs` (a `shared_for(con, doctype, user, ptype) -> Vec<docname>` helper querying `tabDocShare` for `share_doctype=?, user=? OR everyone=1` with the right column set), consumed in `permission()` (doctype-level: at least one share ⇒ allowed) and in the list/get paths (OR the shared names into the result/precheck). `false_if_not_shared` (permissions.py:177-205) is the reference. Lower priority than G4 since it only *under*-grants.

- **G6 — Controller `has_permission` hooks not implemented** — **Severity: Low for pure ferro.**
  `has_controller_permissions` (B22) can veto access; ferro has no hook dispatch (pure Rust has no app controllers). This is the `--features python`/`ferrod` story, not pure ferro. No standalone test isolates this (it's exercised indirectly via `get_doc_permissions`), so impact on the spec suite is minimal. Fix: only meaningful in the ferrod build via pyfall; out of scope for the pure-Rust deliverable.

### Any UNDOCUMENTED ferro behavior discovered (divergences not covered by a test)

- **U1 — Write/PUT/DELETE are owner-pre-checked but the field-write payload is NOT permlevel-masked.**
  `ReadAcl`/permlevel masking (`orm.rs:28-45`) governs only **reads**. On insert/update ferro applies no permlevel-1 *write* protection: a user with only permlevel-0 write could set a permlevel-1 field if it reached the ORM. Frappe blocks writes to higher-permlevel fields the user lacks write on. Not asserted by `test_permissions.py` directly (`test_mask_fields.py` is the relevant suite) but it's a real divergence. Severity Med if any permlevel-1 *scalar* column is writable; in this single-app bench the permlevel-1 User fields are mostly child tables, masking the effect.

- **U2 — JSON key corruption for reserved columns in the create/update response.**
  Probing test1's `POST /api/v2/document/Contact` returned a body with literally-escaped keys: `"\"parent\"":"parent","\"parentfield\"":"parentfield","\"parenttype\"":"parenttype"` (the SQL-quoted identifiers leaked into JSON keys, and their *values* were the literal column names). This is outside the permissions domain (it's a serialization bug in the doc-return path in `orm.rs`/`main.rs`) but was surfaced while exercising the create-not-owner-scoped permission path. Worth a separate ticket.

- **U3 — `only_if_owner` single-doc check treats "doc not found / no owner column" as no violation.**
  `main.rs:1106-1108`: `doc_owner` returning `None` ⇒ `owner_violation` is `false`, so a GET on a nonexistent name under if_owner falls through to `orm::get_doc` which then 404s. Behaviorally fine (you still can't read it), but it means the 403-vs-404 ordering differs from Frappe (Frappe's `getdoc` would PermissionError before existence is confirmed in some paths). Severity Low; cosmetic on error code.

- **U4 — `select`/`report`/`export`/`submit`/`cancel` ptypes are accepted by `permission()` (`auth.rs:189-192`) but unreachable** — `ptype_for_method` (auth.rs:119-127) only ever emits read/create/write/delete from HTTP verbs, so the extra columns in the match arm are dead for the REST surface. Harmless, but means submit/cancel are never permission-checked distinctly (a PUT that performs a submit-like change is gated on `write`, not `submit`).
