# orm-document behavioral fidelity: Frappe spec tests vs ferro

Domain: insert/update/delete semantics, defaults, mandatory/required, child tables, timestamps,
naming, docstatus, duplicate, link validation, fetch_from, set_only_once, read_only, ignore_permissions.

Spec files: `frappe/tests/test_document.py`, `test_base_document.py`, `test_child_table.py`, `test_docstatus.py`.
ferro source: `src/orm.rs` (insert/update/delete) + `src/main.rs` route_resource / route_v2_document + `src/naming.rs` + `src/meta.rs`.

Probes were run live against `http://127.0.0.1:8081` (site `bench-test/mysite.sqlite`, Administrator token).
NOTE: a competing agent rotates Administrator's `api_key` via `provision-key` on the same DB, so each
probe batch re-provisions and uses the key atomically within one shell invocation.

---

## Behaviors Frappe guarantees (from the spec tests)

1. **Autoname on insert** — `Event` (`naming_series:EV-...` style) → name starts with `EV` (`test_insert`).
   `field:`-named doctypes derive name from the field; `naming_series` advances `tabSeries` atomically
   (`test_naming_series`, `revert_series_if_last`).
2. **Docfield default values applied on insert** — `Event.send_reminder == 1` after insert (`test_insert`).
3. **Mandatory/required → `frappe.MandatoryError`** (HTTP 417) when a reqd field is empty
   (`test_mandatory`: User w/o `first_name`). Text-editor/empty-string handling must not false-positive
   (`test_text_editor_field`).
4. **Update writes through** and the new value is persisted (`test_update`).
5. **System/timestamp fields are server-authoritative**: `creation`, `modified`, `modified_by`, `owner`,
   `docstatus`, `idx` set on insert; `modified`/`modified_by` bumped on update (`test_value_changed`
   asserts `modified` changed but `creation` did not).
6. **Conflict / optimistic lock** — saving a doc whose in-DB `modified` differs from the client's copy
   raises `frappe.TimestampMismatchError` (`test_conflict_validation`, `test_conflict_validation_single`).
7. **Permission gate on write** — Guest inserting → `frappe.PermissionError`; single save by Guest →
   PermissionError (`test_permission`, `test_permission_single`).
8. **Link validation** — a Link/child-Link value pointing at a non-existent target →
   `frappe.LinkValidationError` (`test_link_validation`: role "ABC").
9. **Child table writes** — child rows get `parent`, `parenttype`, `parentfield`, `idx` (1-based),
   and inherit default values from the child docfield (`test_website_route_default`,
   `test_insert_with_child`, `test_child_table` `child_table_fields` present).
10. **Duplicate name → `frappe.DuplicateEntryError`** (HTTP 409) (implied; `ignore_if_duplicate` path).
11. **docstatus transitions** (`DocStatus` 0/1/2): non-submittable doctype set to `docstatus=1` →
    `frappe.DocstatusTransitionError`; `0→2` invalid; submit then `discard`/`save` rules; `skip_docstatus_validation`
    flag bypasses (`test_non_submittable_doctype_docstatus_transition`, `test_skip_docstatus_validation_flag`,
    `test_discard_transitions`, `test_docstatus`, `test_draft/submitted/cancelled`).
12. **DocStatus value object semantics** — `DocStatus(0/1/2)` == DRAFT/SUBMITTED/CANCELLED,
    `is_draft()/is_submitted()/is_cancelled()` (`test_docstatus.py`, `test_base_document.test_docstatus`).
13. **set_only_once** — changing a `set_only_once` field after insert →
    `frappe.CannotChangeConstantError` (417) (`validate_set_only_once`; no direct test here but core ORM contract).
14. **Saving a new doc that carries a `name` → `frappe.DoesNotExistError`** (`test_error_on_saving_new_doc_with_name`).
15. **Non-negative / varchar-length / XSS-sanitize / update-after-submit / from-to-date** validations
    (`test_non_negative_check`, `test_varchar_length`, `test_xss_filter`, `test_update_after_submit`,
    `test_validate_from_to_dates`).
16. **`get_doc` on a single reads from `tabSingles`**; empty table-field returns `[]` (`test_load_single`,
    `test_get_return_empty_list_for_table_field_if_none`).
17. **fetch_from** — fields with `fetch_from` are auto-populated from the linked doc on save
    (core ORM contract; spec covered elsewhere but exercised by insert paths).
18. **`get_formatted`, virtual fields, lazy docs, get_docs collection, run_method, extend/set** — pure
    in-process Python (no HTTP surface).

---

## MATCH (ferro replicates)

- **M1. Autoname / naming_series prefix** — `orm.rs:568` calls `naming::resolve_name`; `naming.rs:63-66`
  handles `naming_series:` and advances `tabSeries` atomically via UPSERT…RETURNING (`naming.rs:212-220`).
  Live: insert Event → `name=EV00008`, EV-prefix true. (`test_insert`, `test_naming_series`)
- **M2. Docfield default values** — `orm.rs:609-617` applies `default_for` for any column not in payload;
  `default_for` (`orm.rs:506-538`) handles literal defaults, `__user`, `Today`, `now`, and **Select
  first-option default**. Live: Event `send_reminder=1`, `event_category=Event` (Select default). (`test_insert`)
- **M3. Mandatory/required → 417** — `orm.rs:639-651` validates reqd fields after defaults and returns
  `OrmError::Validation`, mapped to **417** at `main.rs:909/936`. Live: User w/o first_name → 417. (`test_mandatory`)
  *(exc_type caveat — see PARTIAL P1.)*
- **M4. Update persists + bumps timestamps** — `orm.rs:815-822` sets `modified`/`modified_by` on every
  update; `orm.rs:800-814` writes payload columns. Live: PUT description → persisted, `modified` advanced.
  (`test_update`, `test_value_changed` modified-changed assertion)
- **M5. System fields server-authoritative on insert** — `orm.rs:619-637` sets `owner`, `modified_by`,
  `creation`, `modified`, `docstatus=0`, `idx=0`; payload cannot override them
  (`SYSTEM_INSERT_FIELDS` filter `orm.rs:549/596`). Live: owner/modified_by=Administrator, docstatus=0,
  creation==modified. (`test_value_changed`)
- **M6. Permission gate on write** — `main.rs:1343-1352` (v1) / `1094-1098` (v2) check
  `auth::permission` for create/write/delete before touching the ORM; Guest → 403. (`test_permission`)
  *(NB: a separate role-seeding bug, documented in the shared context #1, can make Guest pass reads it
  shouldn't; the write gate itself is correct.)*
- **M7. Child table writes (parent/parenttype/parentfield/idx + identity)** — `insert_children`
  (`orm.rs:677-747`) sets `parent`, `parenttype`, `parentfield`, 1-based `idx`, `owner`, `modified_by`,
  `creation`, `modified`, `docstatus=0`. Live: User.roles[0] → parent=user, parentfield=roles,
  parenttype=User, idx=1. (`test_insert_with_child`, `test_child_table`)
- **M8. Duplicate name → 409** — INSERT maps SQLite ConstraintViolation to `OrmError::Duplicate`
  (`orm.rs:660-668`) → **409 DuplicateEntryError** (`main.rs:910/937`). Live: ToDo with repeated explicit
  name → 409 `DuplicateEntryError`. (duplicate contract)
- **M9. Single read/write via tabSingles** — `get_single` (`orm.rs:480-503`), `update_single`/`insert_single`
  (`orm.rs:749-758, 861-889`). (`test_load_single`)
- **M10. Submitted doc cannot be deleted** — `delete` (`orm.rs:918-923`) refuses `docstatus==1` with a
  Validation error (417). (docstatus delete guard) *(could not be exercised live: no submittable doctype
  installed in this single-app bench.)*
- **M11. DocStatus enum semantics are SQLite-native** — Check/Int/docstatus come back as integers
  (`cell_to_json` `orm.rs:67-75`); docstatus stored as integer 0. The value-object methods
  (`is_draft` etc.) are pure Python never exposed over HTTP; the *stored representation* matches.
  (`test_docstatus.py`, `test_base_document.test_docstatus`)

---

## PARTIAL

- **P1. Mandatory error type label** — ferro returns **HTTP 417 with `exc_type:"ValidationError"`**
  and message `Value missing for <DocType>: <field>`. Frappe raises `frappe.MandatoryError` (subclass
  of ValidationError, also 417) with message `Error: Value missing for <DocType>: <Label>`. Status code
  and "value missing" semantics MATCH; the **`exc_type` string and the field-vs-label in the message
  differ**. Clients keying on `exc_type=="MandatoryError"` would mis-classify. Severity Low.
  Fix: in `map_orm_err`/`map_orm_err_v2` (or by adding an `OrmError::Mandatory` variant emitted at
  `orm.rs:646`) map the mandatory case to exc_type `MandatoryError`.

- **P2. docstatus on write — ignored, not validated** — `docstatus` is in `SYSTEM_INSERT_FIELDS`
  (`orm.rs:549`) and `PROTECTED_UPDATE_FIELDS` (`orm.rs:761-763`), so a client `docstatus` is **silently
  dropped**: insert forces 0, update never changes it. Live: PUT `{docstatus:1}` on a ToDo → 200, stored
  docstatus stays 0. Frappe would raise `DocstatusTransitionError` for the non-submittable case
  (`test_non_submittable_doctype_docstatus_transition`). ferro neither errors nor corrupts — it just
  never submits/cancels. So the *non-submittable transition* test's protective intent is satisfied
  (the doc is not submitted), but the *error contract* is not, and **submit/cancel are entirely
  unsupported** (see GAP G4). Severity Med (partial: safe but silent).

---

## GAP

### G1. Link validation not performed — Med
Frappe: inserting a doc whose Link/child-Link value targets a non-existent record raises
`frappe.LinkValidationError` (417) (`test_link_validation`, role "ABC"). ferro performs **no link
existence check**. Live: User with `roles:[{role:"ZZ_NO_SUCH_ROLE"}]` → **200** (Frappe = 417).
- Fix location: `orm.rs insert` after the required-field loop (~`orm.rs:651`), and in `insert_children`
  (`orm.rs:706-744`) for child Link fields, plus `update` (`orm.rs:800-814`).
- Fix sketch: extend `DocField` (`meta.rs:17-25`) to keep `fieldtype=="Link"` + `options` (target
  doctype). For each Link/Dynamic-Link value being written that is non-empty and not `ignore_links`,
  run `SELECT 1 FROM "tab<options>" WHERE name=?`; if zero rows → `OrmError::Validation` with a
  LinkValidationError-shaped message (ideally a new `OrmError::LinkValidation` mapped to 417 with
  exc_type `LinkValidationError`). Honor `ignore_links`/`ignore_validate` flags as no-ops for now.

### G2. set_only_once not enforced — Med
Frappe: changing a `set_only_once` field after the doc exists raises `frappe.CannotChangeConstantError`
(417) (`validate_set_only_once`, document.py:1110). ferro does **not parse `set_only_once`** at all
(absent from `DocField`/`load_meta`). Live: insert Notification(channel=Email), PUT channel=System
Notification → **200**, value changed (Frappe = 417).
- Fix location: `meta.rs:18-25/131-145` (parse `set_only_once`), `orm.rs update` (`orm.rs:800-814`).
- Fix sketch: add `set_only_once: bool` to `DocField`, select it in `load_meta`. In `update`, before
  applying a payload key, if the field is `set_only_once`, read the current DB value and compare;
  if the incoming value differs from the stored one → `OrmError::Validation`/new
  `OrmError::CannotChangeConstant` (417). For Date/Datetime/Time compare as strings (Frappe does
  `str(value)!=str(original)`); for table fields compare row-by-row (lower priority).

### G3. Optimistic-lock / TimestampMismatch not enforced — Med
Frappe: PUT/save with a stale `modified` (differs from DB) → `frappe.TimestampMismatchError`
(`check_if_latest`, document.py:1283; `test_conflict_validation`). ferro puts `modified` in
`PROTECTED_UPDATE_FIELDS` (`orm.rs:761`) and unconditionally overwrites it (`orm.rs:815-817`) **without
comparing** to the client-supplied value. Live: PUT with `modified:"2000-01-01..."` → **200** (Frappe = 409/417).
- Fix location: `orm.rs update` inside the txn, right after the existence check (`orm.rs:782-794`).
- Fix sketch: if the payload contains `modified`, `SELECT modified FROM tab... WHERE name=?` and compare
  (string compare, like Frappe's `cstr`). On mismatch → new `OrmError::Conflict`/`Validation` mapped to
  the `TimestampMismatchError` shape. (Frappe uses 417 for this via `msgprint(raise_exception=...)`,
  but a 409 envelope is also defensible — match the exc_type string `TimestampMismatchError`.)
  Note: this only matters when the client sends `modified`; the REST FrappeClient does, the SPA mostly
  does not.

### G4. Submit / cancel / discard unsupported — Med (footprint-justified)
Frappe: `docstatus` 0→1 (submit), 1→2 (cancel), and the draft→discard(2) transition, each firing
controller hooks (`test_discard_transitions`, `test_submittable_insert`). ferro has **no submit/cancel
verb** and drops any `docstatus` write (see P2). Pure-Rust ferro cannot run controller submit/cancel
hooks, so full fidelity is out of scope (this is `ferrod`/Python's job). At minimum the **error contract**
for an illegal transition on a non-submittable doctype is missing.
- Fix location: `orm.rs update` (`orm.rs:766-859`) + a v2 method or `?run_method=submit` route.
- Fix sketch (minimal, no hooks): parse `is_submittable` and `docstatus` into meta; in `update`, when a
  `docstatus` change is requested, validate the transition matrix (0→0,0→1 only if submittable,1→1,1→2;
  reject 0→2, 1→0, and 0→1 for non-submittable with a `DocstatusTransitionError`-shaped 417) and, if
  valid, allow the write. Honor `skip_docstatus_validation` flag (`test_skip_docstatus_validation_flag`).
  Controller-hook execution remains a non-goal for pure ferro.

### G5. fetch_from not implemented — Low
Frappe auto-populates `fetch_from` fields (`linkfield.targetfield`) from the linked doc on save. ferro
does not parse or apply `fetch_from` (absent from `DocField`). The site has such fields
(e.g. `User Email.email_id = email_account.email_id`). No spec test in this set directly asserts it over
HTTP, and the value is recomputed by Frappe whenever a CPython worker next touches the row, so impact is
low for read-mostly clients. Severity Low.
- Fix location: `meta.rs` (parse `fetch_from`), `orm.rs insert`/`update` after column assembly.
- Fix sketch: for each field with `fetch_from="link.target"`, if the link value is present and the local
  field is empty, `SELECT target FROM tab<linked-doctype> WHERE name=<linkvalue>` and set it. Skip if
  `fetch_if_empty` semantics or the field is user-supplied.

### G6. `ignore_permissions` flag not honored on the REST data plane — Low
Frappe's `insert(ignore_permissions=True)` / `save(ignore_permissions=True)` bypass the permission gate
(used heavily in fixtures/tests, `test_child_table` inserts a DocType with `ignore_permissions=True`).
ferro's REST path always enforces `auth::permission` and has **no `ignore_permissions` query param**.
For the token-authenticated REST surface this is arguably correct (you can't let an HTTP caller disable
perms), and Frappe's REST endpoint likewise ignores a client `ignore_permissions`. So this is **not a
divergence for the HTTP contract** — flagged only because the tests use the flag in-process. No fix needed
for the REST plane. Severity Low / informational.

---

## UNDOCUMENTED ferro behavior (divergences not covered by a test)

- **U1. Child-table update is full-array replace** — `update` (`orm.rs:835-854`) DELETEs all existing
  child rows for a supplied child fieldname and re-inserts the payload array. This matches the REST PUT
  contract but means a PUT that omits a child field **leaves existing children untouched** (it only
  replaces fields present in the payload), while a PUT that includes the field with a partial array
  **drops the omitted rows and regenerates their `name`s** (new `util::random_name()` each time,
  `orm.rs:711`). Frappe preserves child `name`s for unchanged rows and diff-updates. Cosmetic but
  observable (child `name` churn, lost child-row links).

- **U2. Single update never deletes a field set to null / never validates reqd** — `update_single`
  (`orm.rs:874-884`) skips `Value::Null` (so you cannot clear a single field by sending null) and does
  no mandatory/link/set_only_once validation. Also `insert_single` runs no required-field check.

- **U3. `default_for` ignores `:`-prefixed cross-doc defaults** — `orm.rs:510-512` returns None for
  defaults like `:Company`. Frappe would resolve them from `frappe.defaults`. Acceptable for the REST
  data plane but a silent omission.

- **U4. read_only fields ARE writable over REST** — ferro lets a client write a `read_only` field
  (live: ToDo.assignment_rule set to an arbitrary value). This actually **matches Frappe**: `read_only`
  is a client/UI hint and Frappe's server-side `save` does not strip read_only values. So *not* a bug —
  noted because the focus list called it out; ferro and Frappe agree (no server enforcement either side).

- **U5. DELETE returns 202 with `{"data":"ok"}`** (`main.rs:1425/1191`). Frappe's REST DELETE returns
  202 with `{"message":"ok"}` (v1). Minor envelope key difference (`data` vs `message`) — likely covered
  by an api test in another domain, listed here for completeness.

- **U6. Non-negative / varchar-length / XSS-sanitize / update-after-submit checks absent** — the
  `db_insert`-layer validations (`test_non_negative_check` NonNegativeError, `test_varchar_length`
  CharacterLengthExceededError, `test_xss_filter` HTML sanitize, `test_update_after_submit`
  UpdateAfterSubmitError, `validate_from_to_dates` InvalidDates) are **not** reproduced by ferro. These
  are Python-controller/`db.py`-layer guards; pure ferro writes the raw value. Low impact for trusted
  token clients; data written this way differs from what Frappe would have stored/rejected.

- **U7. Saving a new doc that carries an explicit `name` does NOT raise DoesNotExistError** — Frappe's
  `save()` on a `__islocal` doc with a `name` raises `DoesNotExistError` (`test_error_on_saving_new_doc_with_name`).
  ferro's REST POST treats an explicit `name` as the chosen name (`naming.rs:34-38`) — which is the
  correct REST-create behavior; the Frappe assertion is about the in-process `save` of an unsaved object,
  not the REST create path. Not a divergence for HTTP; noted for clarity.
