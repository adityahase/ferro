# 08 — Implementation status (fixes applied)

This file records what was actually implemented from `06-fix-plan.md` (+ the behavioral appendix),
with the regression lock that guards each. All locks live in `measurements/verify.py`
(in-process `ferro request` harness) unless noted; the suite is **102 passing, 0 failing** and
deterministic across runs. Each fix is its own focused commit on `fix/signup-apps-render`.

## Applied

| Fix | What | Lock |
|---|---|---|
| **FIX-1** | `auth::user_roles` matches `get_roles`: Guest→`["Guest"]` only; users get HasRole−AUTOMATIC + `["All","Guest"]` (+`Desk User` for System Users) | Guest `GET /api/resource/ToDo` → 403; System user still reads ToDo |
| **FIX-9** | `frappe.client.*` reads gated like `/api/resource` (perm + readable_permlevels + if_owner); `client.get` null-strips | Guest `client.get_list/get_value/get_count User` → 403; admin still 200; permlevel mask holds |
| **FIX-7** | `--desk` no longer silently runs as Administrator; admin-default is an explicit `--insecure-desk-admin` (or `--default-user`) opt-in + warning | live boot: `--desk` → Guest+warn; `--insecure-desk-admin` → Administrator |
| **FIX-8** | meta columns come from PRAGMA for real tables (parent/parentfield/parenttype only on child tables); `legacy_double_quoted_strings=OFF` | User has no `parent*` phantom keys; child doc keeps real parent linkage |
| **B-NAM-1** | ISO-8601 `WW` week (`util::iso_week` + Jan/Dec fixup) replacing `(doy+6)/7` | Rust unit tests vs CPython `strftime("%V")` vectors |
| **B-NAM-6** | `{timestamp}` → `now()` microsecond precision (no same-second name collisions) | two rapid Console Log creates get distinct names |
| **B-NAM-2** | child rows keep a client/existing name, else follow the child DocType's autoname (not always hash) | (covered by save path; child-link validation lock) |
| **B-FIL-1/2/3** | dict elements in filters/or_filters; strip `None` from in/not-in; JSON-encoded list string | 5 locks (JSON-list, not-in-null→all, in-null→none, dict-in-list, or_filters dicts) |
| **B-DB-1/2** | Single doctype reads from tabSingles (get_value/get_list); multi-field get_value; get_single_value casts + null-not-0 | 5 locks |
| **B-MET-1/2** | merge tabCustom Field; apply tabProperty Setter (naming/sort + field fieldtype/options/reqd/default/permlevel/set_only_once) | 4 locks (custom-field permlevel mask; property-setter permlevel override) |
| **B-DOC-1** | Link / Dynamic Link target existence → 417 (insert, update, child rows) | bad link → 417, good link → 200 |
| **B-DOC-2** | `set_only_once` enforced on update → 417 | changing a set_only_once field → 417, other fields OK |
| **B-DOC-3** | optimistic lock: client `modified` must match DB → 417 | stale modified → 417, current → 200 |
| **B-REST-1** | `/api/v2/*` auth failures use the v2 `errors[]` envelope (not v1 `exc_type`) | v2 401 errors[]; v1 401 exc_type |
| **B-REST-2** | `/api/v2/doctype/<dt>/meta` (only_for All) + `/count` (read-gated) | meta has fields[]; count is int; Guest meta → 403 |
| **B-REST-3** | `/api/v2/document/<dt>/<name>/copy` strips identity fields, marks `__islocal` | copy has no name, `__islocal=1` |
| **B-REST-4** | `maintenance_mode` → 503 `InReadOnlyMode` on writes (v1+v2), reads still serve | write→503, read→200 (flag toggled around the probe) |
| **FIX-3** | `expand` (list) / `expand_links` (single) replace link values with the linked doc (ACL-gated), v1+v2 | single + list expansion locks |
| **FIX-4** | v2 `document/<dt>/bulk_delete` (names) + `method/bulk_delete`/`bulk_update` (docs) with summary; 417 on non-list | 4 locks |
| **FIX-5** | `frappe.realtime.get_user_info` stub; `frappe.client.set_value` / `delete` write methods | get_user_info; set_value single+dict; delete; Guest set_value→403 |
| **FIX-6** | `debug=1` → `_debug_messages` on list responses | key present |
| **FIX-2** | honest `login` (verify pwd via passlib) + real `tabSessions` sid (mint on login, validate cookie, expiry, logout) | wrong pwd→401, correct→200+user; **live**: sid cookie → credentialed user |
| **perms** | write-path permlevel masking; DocShare single-doc grants | permlevel-1 field masked on write; shared Role readable, non-shared 403 |

`revert_series_if_last` (B-NAM-3) is effectively covered: ferro's writes run inside an IMMEDIATE
transaction, so a failed insert rolls back the `tabSeries` counter increment atomically — no
separate revert needed.

## Deliberately scoped out (minimalism directive: "as few changes as possible, don't reimplement
unnecessary parts, keep the memory footprint low")

- **User Permissions** (doc-level link constraints via `tabUser Permission`). A large subsystem
  (per-link-field WHERE constraints + `apply_user_permissions`/`strict_user_permissions`/ignore
  flags). A partial implementation risks **over-restricting** the live signup deployment, which is
  worse than the documented over-grant. Left out by design; DocShare (the under-grant side) IS done.
- **B-MET-3** meta-cache invalidation. ferro provisions schema at new-site/install/migrate time and
  caches Meta per-process; runtime DocType/Custom Field/Property Setter edits aren't observed until
  restart. Acceptable for the deployment model; adding a TTL/invalidation bus is out of scope.
- **B-DB-3** db-api extras: aggregates (`{"COUNT":f}`), `distinct`/`group_by`/`pluck`/`as_list`,
  field alias, `run=False`. Low; frappe-ui doesn't depend on them for the served SPAs.
- **B-NAM-4** `append_number_if_name_exists` (Bottle→Bottle-1). Frappe only appends in narrow naming
  paths; ferro returns the correct `409 DuplicateEntryError` for the general case. Low.
- **submit/cancel/docstatus transitions** and controller `has_permission` hooks remain the `ferrod`
  (embedded-Python) tier's job, not pure ferro — as the audit concluded.
