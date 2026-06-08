# ferro — known limitations / intentionally-deferred divergences

ferro targets the Frappe **v1 REST data plane** (CRUD + token auth + doctype/row/field
permissions) as a memory-light drop-in. The items below are confirmed differences from a full
CPython+Frappe worker that are out of that scope or deliberately deferred. None are crashes or
silent data corruption; each is a behavioral gap a client could observe.

## By design (a REST runtime, not the framework)
- **No controller business logic.** Frappe `validate`/`before_save`/`on_submit` hooks, server
  scripts, and whitelisted controller methods do not run. `POST /api/resource/<dt>/<name>`
  (run_method / submit-cancel) returns 405. Computed/fetched fields, doc-event side effects,
  and custom `get_list` query rewrites are not applied.
- **No `/api/v2` namespace.** Only v1 (`/api/resource`, `/api/method`) is served. v2's
  `has_next_page`, bulk ops, `/document`, `/doctype/<dt>/{meta,count}` are absent.
- **`/api/method` is allow-listed** to `ping` + `get_logged_user` (any other dotted method → 404),
  since arbitrary methods are Python.

## Permissions (partial parity)
- **`if_owner` is implemented; full User Permissions are not.** Per-user link-value restrictions
  (`tabUser Permission`), `apply_user_permissions`, strict-vs-empty allowances, and DocShare
  (`tab DocShare`) row sharing are not enforced. Effect: a restricted user may see/edit rows that
  Frappe's User-Permission match conditions would hide (beyond the owner scope, which *is* honored).
- **Field-level permlevel is enforced on reads, not fully on writes.** Reads mask permlevel>0
  fields the user can't read; writes guard system fields and reject unknown/unreadable fields, but
  do not reset a permlevel>1 field a user lacks write access to as precisely as Frappe.

## Write semantics
- **Child-table update is full-array replace** (delete + reinsert), so child row `name`s change.
  Frappe does a name-preserving diff. Matters only if external references target child row names.
- **Single update is a partial merge** (PATCH-style), not Frappe's full delete-and-reinsert of the
  single's stored fields.
- **No link-integrity check on delete.** Frappe raises `LinkExistsError` when other docs reference
  the target; ferro deletes it (FK enforcement is off, matching Frappe's app-level approach — but
  the app-level check itself is not reimplemented).
- **No optimistic-concurrency guard** (`check_if_latest` / `modified` timestamp): concurrent
  updates last-writer-wins instead of raising a conflict.
- **`autoincrement` uses MAX(name)+1** within the write transaction rather than a DB sequence.

## Auth
- **Basic auth supports `api_key:api_secret` only**, not interactive username/password (Frappe
  hashes login passwords with passlib/pbkdf2, a separate scheme from the Fernet api_secret path).

## Query
- **Missing exotic operators:** `timespan`/`previous`/`next`, NestedSet `descendants of`/`ancestors
  of`. Implemented: `= != > < >= <= <>`, `like`/`not like`, `in`/`not in` (incl. comma-strings),
  `between`, `is set`/`is not set`.
- **No joined/child-field filters** (`tabChild.field` / `field.subfield`) or `group_by`/aggregate
  selects; filters/fields/order_by operate on the doctype's own physical columns.
- **`fields=["*"]`** expands to physical (permlevel-readable) columns; it does not include computed
  or child fields the way a full `as_dict()` might.

## Misc
- **Unbounded result sets** (`limit_page_length=0` on a huge table) build the full result in RAM
  before serializing — same non-streaming behavior as Frappe; bounded in practice by the 8 MiB
  request cap on input but not on output size.
- **Type coercion** for non-single reads returns raw SQLite affinity; Frappe additionally casts a
  few fieldtypes in `as_dict` (e.g. v2 stringifies integer Link values). Singles ARE cast by ferro.
