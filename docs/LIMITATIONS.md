# ferro — known limitations / intentionally-deferred divergences

ferro targets the Frappe **v1 + v2 REST data plane** (CRUD + token/password auth + sessions +
doctype/row/field permissions) as a memory-light drop-in. The items below are confirmed differences
from a full CPython+Frappe worker that are out of that scope or deliberately deferred. None are
crashes or silent data corruption; each is a behavioral gap a client could observe.

## By design (a REST runtime, not the framework)
- **No controller business logic.** Frappe `validate`/`before_save`/`on_submit` hooks, server
  scripts, and whitelisted controller methods do not run. `POST /api/resource/<dt>/<name>`
  (run_method / submit-cancel) returns 405. Computed/fetched fields, doc-event side effects,
  and custom `get_list` query rewrites are not applied.
- **`/api/v2` is served** (`/document`, `/doctype/<dt>/{meta,count}`, bulk delete/update, copy,
  `expand`, the v2 `errors[]` envelope). Still absent: `has_next_page` pagination cursors.
- **`/api/method` serves a fixed surface** — the perm-gated `frappe.client.*` (get/get_list/
  get_value/get_count/set_value/delete) and `frappe.desk.*` reads/saves the Desk SPA needs, plus
  `ping`/`get_logged_user`. Arbitrary app `*.api.*` methods are Python and need the `ferrod` tier.

## Permissions (partial parity)
- **`if_owner` and DocShare are implemented; doc-level User Permissions are not.** Owner scoping and
  DocShare single-doc grants (`tabDocShare` — the under-grant side) *are* enforced. Per-user
  link-value restrictions (`tabUser Permission`), `apply_user_permissions`, and strict-vs-empty
  allowances are not. Effect: a restricted user may see/edit rows that Frappe's User-Permission match
  conditions would hide (beyond owner + share scope, which *are* honored). Left out by design: a
  partial implementation risks over-restricting, which is worse than the documented over-grant.
- **Field-level permlevel is enforced on both reads and writes.** Reads mask permlevel>0 fields the
  user can't read; writes mask fields whose permlevel the user lacks write access to (so a forged
  payload can't set them). The one gap vs Frappe is meta-cache invalidation: permlevel changes made
  at runtime aren't observed until the worker restarts.

## Write semantics
- **Child-table update is full-array replace** (delete + reinsert), so child row `name`s change.
  Frappe does a name-preserving diff. Matters only if external references target child row names.
- **Single update is a partial merge** (PATCH-style), not Frappe's full delete-and-reinsert of the
  single's stored fields.
- **No link-integrity check on delete.** Frappe raises `LinkExistsError` when other docs reference
  the target; ferro deletes it (FK enforcement is off, matching Frappe's app-level approach — but
  the app-level check itself is not reimplemented). Link *target* existence on insert/update *is*
  now validated (`417` on a dangling Link / Dynamic Link).
- **`autoincrement` uses MAX(name)+1** within the write transaction rather than a DB sequence.

## Auth
- **Password login + sessions are implemented.** `POST /api/method/login` verifies the password
  (passlib/pbkdf2, the same hash Frappe stores) and mints a real `tabSessions` sid cookie (validated
  per request, expiry-checked, re-checks the user is still enabled, cleared on logout); `api_key:
  api_secret` Basic auth also works. **Not** implemented: OAuth / social login, LDAP, and 2FA.

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
