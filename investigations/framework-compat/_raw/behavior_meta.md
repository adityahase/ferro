# Behavioral fidelity: `meta` domain (Frappe spec tests ↔ ferro `src/meta.rs`)

## meta

**Note on spec sources.** There is **no** `frappe/tests/test_meta.py` in this bench. The meta
contract is specified (a) in the model module `frappe/model/meta.py` (the `Meta` class + module
constants) and `frappe/model/__init__.py` (canonical field/type lists), and (b) is *asserted* by
tests in `frappe/core/doctype/doctype/test_doctype.py` plus references in `test_document.py`.
Behaviors below are extracted from those concrete assertions and from the module API that ferro's
read/write path depends on. ferro is a pure-Rust HTTP runtime; meta is exercised only indirectly
(every `/api/resource`, `/api/v2/document` call loads meta). Probes were run against the live
ferro server at `http://127.0.0.1:8081`.

ferro source under test: `/home/frappe/ferro/src/meta.rs` (load_meta, Meta, DocField, MetaCache),
consumed by `src/orm.rs` and `src/desk.rs`.

---

### Behaviors Frappe guarantees (bullet list)

- **B1 Meta load from `tabDocType` + `tabDocField`.** `Meta(doctype)` loads the DocType row and its
  DocFields (ordered) — `frappe.model.meta.Meta.__init__` / `load_from_db`.
- **B2 default_fields / standard columns.** Every table has `default_fields` = `(doctype, name,
  owner, creation, modified, modified_by, docstatus, idx)` and optional columns `_user_tags,
  _comments, _assign, _liked_by, _seen`, plus child tables add `(parent, parentfield, parenttype)`.
  (`frappe/model/__init__.py:82-99`, `meta._valid_columns`).
- **B3 `get_valid_columns()` = default_fields + docfields whose `fieldtype in data_fieldtypes` and
  not `is_virtual`, + child_table_fields if `istable`** (`meta.py:263-277`). This is the SQL-column
  set used to validate/guard every fieldname touched by a query.
- **B4 Fieldtypes: `data_fieldtypes` vs `no_value_fields`.** `no_value_fields` = Section Break,
  Column Break, Tab Break, HTML, Table, Table MultiSelect, Button, Image, Fold, Heading — these have
  NO column in the parent table. All other listed types are storable columns
  (`frappe/model/__init__.py:8-65`).
- **B5 `get_table_fields()` / child meta.** Fields whose fieldtype ∈ `("Table","Table MultiSelect")`
  are child tables; their `options` names the child DocType (`meta._table_fields`, `_table_doctypes`,
  `meta.py:514-534`). `get_doc` must expand child rows from the child table keyed by
  `parent/parenttype/parentfield`.
- **B6 `get_field(fieldname)` / `has_field`.** Return the docfield (or None) / membership
  (`meta.py:295-303`). `test_custom_field_deletion` and `test_delete_doctype_with_customization`
  assert `frappe.get_meta(dt).get_field(f)` reflects current schema.
- **B7 `get_options(fieldname)`** returns the field's `options` (`meta.py:315`). Used for link
  targets and child-table doctype resolution (`test_document.py:679`).
- **B8 Virtual fields / virtual DocType.** `is_virtual` DocType has **no DB table**
  (`test_create_virtual_doctype` asserts `table_exists` is False, `is_virtual==1`). A field with
  `is_virtual` is excluded from `valid_columns`. A child table whose options is a virtual doctype
  can only live under a virtual parent (`test_create_virtual_doctype_as_child_table`).
- **B9 Single DocType meta.** `issingle` DocTypes store data in `tabSingles(doctype, field, value)`,
  not a `tab<DocType>` table; meta still carries the docfields (`is_single_doctype`,
  `delete_fields` Singles branch `model/__init__.py:187`).
- **B10 sort_field / sort_order.** Meta carries `sort_field` (default `modified`) + `sort_order`
  (default `DESC`) used for default list ordering.
- **B11 autoname / naming_rule / title_field / istable / is_submittable** carried on meta
  (`test_json_field`, `test_meta_serialization`, naming tests).
- **B12 JSON fieldtype round-trips.** `JSON` is a real `data_fieldtype` and stores/loads as a column
  (`test_json_field`).
- **B13 Custom Fields merged into meta.** `add_custom_fields` extends `meta.fields` with rows from
  `tabCustom Field` for the doctype (`meta.py:404-420`); their defaults/permlevel/options take
  effect. `test_delete_doctype_with_customization` asserts a custom field survives + its
  property-setter default appears in meta.
- **B14 Property Setters applied to meta.** `apply_property_setters` overrides DocType/DocField
  props from `tabProperty Setter` (`meta.py:422-462`); `test_delete_doctype_with_customization`
  asserts `get_meta(dt).get_field(field).default == "DELETETHIS"` from a property setter.
- **B15 Permissions in meta + Custom DocPerm override.** `meta.permissions` is `tabDocPerm`, replaced
  by `tabCustom DocPerm` when present, **except** for special metadata doctypes (DocType, DocField,
  DocPerm, Custom DocPerm) (`set_custom_permissions`, `meta.py:627-640`). `high_permlevel_fields` =
  docfields with `permlevel>0` (`meta.py:673-675`).
- **B16 get_meta caching + invalidation.** `frappe.get_meta` is cached per-doctype
  (`doctype_meta::<dt>`), and the cache is **cleared on DocType/DocField/customization change**.
  `test_delete_doc_clears_cache` asserts that after `delete_doc("DocType", dt)` a fresh `get_meta(dt)`
  raises `DoesNotExistError` (i.e. the stale meta is gone). `test_custom_field_deletion` asserts a
  deleted child-doctype's Table field disappears from the parent's meta.
- **B17 special_doctypes loaded from JSON file, not DB** (DocField/DocPerm/DocType/Module Def/...) to
  break the circular dependency (`meta.load_from_db`, `meta.py:157-164`).

---

### MATCH (ferro replicates)

- **B1 — MATCH.** `load_meta()` selects the DocType row from `tabDocType` and DocFields from
  `tabDocField` ordered by `idx` (matching Frappe field order). `src/meta.rs:98-152`.
- **B2 — MATCH.** `STANDARD_COLUMNS` = name, creation, modified, modified_by, owner, docstatus, idx,
  parent, parentfield, parenttype, _user_tags, _comments, _assign, _liked_by (`src/meta.rs:12-15`),
  seeded into `columns` for every meta (`src/meta.rs:156`). Probe: selecting
  `name,_assign,_comments,owner,docstatus,idx` on ToDo returns 200 with all columns.
  (Minor omission `_seen` — see GAP G6.)
- **B3 — MATCH (functional equivalent).** ferro builds `columns` as STANDARD_COLUMNS + non-virtual
  docfields + the **authoritative PRAGMA table_info** column set (`src/meta.rs:156-170`), then
  guards every queried fieldname via `meta.has_column()` (`src/orm.rs:149,274,313,604`,
  `src/main.rs:1121`). Probes confirmed: unknown field in `fields` → `ValidationError "Unknown field
  'bogus_field' for ToDo"`; unknown field in `filters` → `ValidationError "Unknown field 'bogus_col'
  in filters for ToDo"`. This is the SQL-injection guard Frappe's `get_valid_columns` exists for, and
  ferro's PRAGMA basis is *more* authoritative than Frappe's (it includes custom columns
  automatically). See PARTIAL P1 for the data_fieldtype-vs-physical distinction.
- **B4 — MATCH.** `DocField::is_virtual_column()` excludes Table, Table MultiSelect, Section Break,
  Column Break, Tab Break, HTML, Heading, Button, Fold from physical columns (`src/meta.rs:32-38`),
  so these fieldtypes never become selectable columns. Probe: selecting `roles` (Table) on User →
  `ValidationError "Unknown field 'roles' for User"`. (Diff vs Frappe `no_value_fields`: ferro's set
  omits `Image` — see GAP G5.)
- **B5 — MATCH.** `DocField::is_child_table()` (Table / Table MultiSelect), `Meta::child_tables()`
  iterator (`src/meta.rs:28-30,67-69`); `get_doc` expands each child table from
  `tab<options>` filtered by `parent=name AND parenttype=meta.name AND parentfield=cf.fieldname ORDER
  BY idx` (`src/orm.rs:441-475`). Probe: `GET /api/resource/User/Administrator` returns the `roles`
  child array. options drives the child DocType via `cf.options` (`src/orm.rs:445`).
- **B6 — MATCH.** `Meta::field(name)` returns the docfield (`src/meta.rs:64-66`), used throughout the
  ORM (`src/orm.rs:41,496,599,804,836`). `has_column` covers `has_field` for the query guard.
- **B7 — MATCH (for child tables).** `cf.options` is read to resolve child DocType
  (`src/orm.rs:445`). Link-option / `get_options` for non-table fields is not surfaced over the data
  plane (read returns raw link value; see context finding #4 expand), but the meta value is present
  on `DocField::options` (`src/meta.rs:21`).
- **B8 — MATCH.** `is_virtual` loaded from `tabDocType` (`src/meta.rs:46,110,123`); virtual meta
  synthesizes columns from docfields only (skips PRAGMA, `src/meta.rs:162`); `get_doc`/`update`/
  `delete` on virtual return NotFound and `insert` returns Validation "Cannot create virtual DocType"
  (`src/orm.rs:296,405-407,562-563,778,897`). Probe: `GET /api/resource/Recorder Suggested
  Index/anything` → `DoesNotExistError` (virtual has no table; correct). `is_virtual` filter list
  works (returns virtual DocTypes).
- **B9 — MATCH.** `issingle` loaded (`src/meta.rs:45,108`); singles read from `tabSingles`
  (`src/orm.rs:402-403,480-498`); single update/delete route to `tabSingles`
  (`src/orm.rs:774,893-895`); single meta synthesizes columns from docfields (no PRAGMA,
  `src/meta.rs:162`). Probe: `GET /api/resource/System Settings/System Settings` returns the
  field/value-assembled single doc with docfield values (app_name, date_format, etc.). (Bare
  `/api/resource/System Settings` without docname errors — see UNDOCUMENTED U1; that's an
  ORM list-routing nuance, not a meta defect.)
- **B10 — MATCH.** `sort_field` (COALESCE default `modified`) + `sort_order` (COALESCE default
  `DESC`) loaded from `tabDocType` (`src/meta.rs:102-104,114-115`) and applied to default ordering
  (`src/orm.rs:358-359`).
- **B11 — MATCH (the load-bearing subset).** `autoname`, `naming_rule`, `title_field`, `istable`
  loaded (`src/meta.rs:43-53,123`). autoname feeds `src/naming.rs`. (`is_submittable` is not loaded;
  see GAP G7.)
- **B12 — MATCH.** `JSON` is not in `is_virtual_column`, so it becomes a normal column and round-trips
  as text through the generic cell read/write path (`src/meta.rs:32-38`; `cast_by_fieldtype` leaves
  JSON as-is). Equivalent to `test_json_field` storing/loading the column.
- **B15 (partial-of-perms) — MATCH for the DocPerm/Custom DocPerm selection + special exclusion.**
  Permission application lives in `src/auth.rs`, not `meta.rs`: `permission_table()` picks
  `tabCustom DocPerm` when any Custom DocPerm row exists for the doctype else `tabDocPerm`
  (`src/auth.rs:162-176`), and the 4 special doctypes are excluded
  (`src/auth.rs:137-140`). `permlevel` field-scoping is honored via `DocField::permlevel`
  (`src/meta.rs:24`) + `readable_permlevels`/`can_read` (`src/orm.rs:41,149,274,411`). This matches
  the substantive `set_custom_permissions` + `high_permlevel_fields` contract for the data plane.
- **B17 — N/A by data (functional MATCH).** ferro reads special doctypes (DocType, DocField, DocPerm)
  directly from their `tab*` tables, which **do** exist in a normal site, so there's no circular
  dependency to break and no file-fallback needed. Probe: `GET /api/resource/DocType?fields=...`
  returns rows. Frappe's file-fallback only triggers when the DB row is missing (install). Same
  observable result on a populated site.

---

### PARTIAL

- **P1 — `get_valid_columns` basis (data_fieldtype filter vs PRAGMA).** Frappe's `_valid_columns`
  *computes* the set from `default_fields + [df for df in fields if df.fieldtype in data_fieldtypes
  and not is_virtual]` (`meta.py:263-277`). ferro instead trusts `PRAGMA table_info` as the
  authoritative physical set (`src/meta.rs:162-170`) and unions in non-virtual docfields. Result is
  *equivalent or stricter* in the common case (PRAGMA = what actually exists), and ferro's guard
  rejects unknown fields correctly (probed). Divergence only at the edges: (a) a docfield of a
  `data_fieldtype` that has **not yet been synced** to a column would be in Frappe's valid_columns but
  not ferro's (ferro requires the physical column); (b) ferro will *accept* a physically-present
  column that Frappe's computed set would exclude (e.g. a leftover trimmed column or an `is_virtual`
  field that still has a stale physical column — ferro can't tell because it doesn't load docfield
  `is_virtual`, see G1). Severity Low for read fidelity; the SQL guard is intact.

- **P2 — Single read field set.** ferro's single read (`get_single`) returns every
  `tabSingles(field,value)` row the user can read (`src/orm.rs:480-498`), which matches Frappe's
  data, but it does **not** synthesize meta defaults for fields absent from `tabSingles`, and casts
  values by docfield fieldtype only when the field is in meta. Substantively matches for stored
  singles; minor divergence on never-set fields (Frappe returns docfield default; ferro omits/null).
  Severity Low.

---

### GAP

- **G1 — Custom Fields not merged into `meta.fields` — Severity: Med.**
  Frappe's `add_custom_fields` (`meta.py:404-420`) appends `tabCustom Field` rows to `meta.fields`
  so their `default`, `permlevel`, `options`, `reqd`, and *child-table* nature take effect. ferro
  only learns custom **columns** via PRAGMA (`src/meta.rs:162-170`) — the custom *docfield* never
  enters `meta.fields`. Consequences: a custom Table field is invisible to `meta.child_tables()` (so
  `get_doc` won't expand a custom child table — relevant to `test_custom_field_deletion`'s child-table
  scenario); a custom field's `default`/`reqd`/`permlevel`/`options` are ignored on insert and
  permission masking. Not exercised in this single-app bench (0 Custom Field rows), so it's latent,
  but it is a real meta-contract divergence.
  **Fix location:** `src/meta.rs` `load_meta()`, after the `tabDocField` loop (~line 152).
  **Sketch:** if `tabCustom Field` exists, run
  `SELECT fieldname, COALESCE(fieldtype,'Data'), options, COALESCE(reqd,0), "default", COALESCE(permlevel,0) FROM "tabCustom Field" WHERE dt = ?1 ORDER BY idx`
  and push each into `fields` (mark them custom). Same SELECT shape as the DocField query; gate on a
  `table_exists("tabCustom Field")` check.

- **G2 — Property Setters not applied — Severity: Med.**
  `apply_property_setters` (`meta.py:422-462`) overrides DocType-level props (autoname, sort_field,
  istable, etc.) and DocField props (default, options, reqd, permlevel, hidden, ...) from
  `tabProperty Setter`. ferro never reads `tabProperty Setter`. So a site that customized e.g. a
  field default or sort_order via Property Setter will see ferro use the *base* JSON value.
  `test_delete_doctype_with_customization` asserts `get_meta(dt).get_field(field).default ==
  "DELETETHIS"` (a property setter) — ferro would return the base default. Not exercised in this
  bench (0 Property Setter rows), latent.
  **Fix location:** `src/meta.rs` `load_meta()`, after fields + custom fields are loaded (~line 170,
  before building `columns`).
  **Sketch:** if `tabProperty Setter` exists, fetch
  `SELECT doctype_or_field, field_name, property, property_type, value FROM "tabProperty Setter" WHERE doc_type = ?1`;
  for `doctype_or_field='DocType'` mutate the relevant Meta scalar (sort_field/sort_order/autoname/...);
  for `'DocField'` find `fields.iter_mut().find(|f| f.fieldname == field_name)` and overwrite the
  matching prop (default/options/reqd/permlevel/fieldtype). Cast `value` per `property_type`.

- **G3 — No meta-cache invalidation API — Severity: Med (High if DocType edits are expected at
  runtime).**
  `MetaCache` exposes only `new`, `get`, `len` (`src/meta.rs:201-245`); there is **no**
  `remove`/`clear`/`invalidate`, and **no caller** drops a cached meta after a DocType / DocField /
  Custom Field / Property Setter write. Frappe guarantees `get_meta` reflects schema changes:
  `test_delete_doc_clears_cache` expects a fresh `get_meta(dt)` to raise `DoesNotExistError` after the
  DocType is deleted; `test_custom_field_deletion` expects the deleted Table field to vanish from the
  parent's meta. ferro would serve the **stale** Meta (cap 512, `src/main.rs:401,661`) until eviction
  by churn or process restart. ferro never offers DocType CRUD as a hot path so the practical blast
  radius is small, but the cache *can never be told a doctype changed*.
  **Fix location:** `src/meta.rs` add to `impl MetaCache`; call sites in `src/orm.rs` write paths and
  `src/desk.rs` save paths when the written doctype is `DocType`/`DocField`/`Custom Field`/`Property
  Setter`.
  **Sketch:**
  ```rust
  pub fn invalidate(&self, doctype: &str) {
      let mut inner = self.inner.lock().unwrap();
      inner.map.remove(doctype);
      if let Some(p) = inner.order.iter().position(|k| k == doctype) { inner.order.remove(p); }
  }
  pub fn clear(&self) { let mut i = self.inner.lock().unwrap(); i.map.clear(); i.order.clear(); }
  ```
  Then in `orm::insert/update/delete` (and `desk::persist_*`): when `meta.name == "DocType"`,
  invalidate the affected doctype name (the doc's `name`); when `meta.name` is `DocField`/`Custom
  Field`/`Property Setter`, invalidate the row's `parent`/`dt`/`doc_type`. Simplest safe version:
  `clear()` whenever any of those four doctypes is written.

- **G4 — `_seen` not in STANDARD_COLUMNS — Severity: Low.**
  Frappe's `optional_fields` includes `_seen` (`model/__init__.py:96`), a real column on many tables.
  ferro's `STANDARD_COLUMNS` (`src/meta.rs:12-15`) omits `_seen`. For physical tables PRAGMA recovers
  it, but for **singles/virtual** (which skip PRAGMA, `src/meta.rs:162`) `_seen` would not be a valid
  column — minor, since singles rarely carry `_seen`.
  **Fix:** add `"_seen"` to `STANDARD_COLUMNS` at `src/meta.rs:14`.

- **G5 — `Image` fieldtype missing from `is_virtual_column` — Severity: Low.**
  Frappe's `no_value_fields` includes `Image` (a display-only fieldtype with no column,
  `model/__init__.py:53-64`). ferro's `is_virtual_column()` (`src/meta.rs:32-38`) omits `Image`, so a
  docfield of fieldtype `Image` would be (wrongly) inserted into the synthesized `columns` for a
  single/virtual doctype. For physical tables PRAGMA is authoritative so no harm; only single/virtual
  with an Image field diverge. Also note Frappe's `display_fieldtypes` is the same idea.
  **Fix:** add `"Image"` to the `matches!` arm at `src/meta.rs:34-37`.

- **G6 — `is_submittable` / `docstatus` semantics not in meta — Severity: Low.**
  Meta doesn't load `is_submittable`. `delete` blindly checks `has_column("docstatus")` for the
  cancelled-state guard (`src/orm.rs:907`) rather than `is_submittable`, and writes never enforce
  submit/cancel transitions (`docstatus` is, however, carried as a standard column). `test_meta_serialization` builds an `is_submittable=1` doctype and
  submits a doc — ferro has no notion of submission. Out of scope for the data plane per project
  priorities, but it is a meta field Frappe carries.
  **Fix (if desired):** add `is_submittable: bool` to `Meta`, load it from `tabDocType` at
  `src/meta.rs:102-115`, and gate the cancel logic on it.

---

### UNDOCUMENTED ferro behavior (divergences not covered by a test)

- **U1 — Single read requires the `/<Doctype>/<Doctype>` form; bare `/api/resource/<Single>` and
  `/api/v2/document/<Single>` error.** `GET /api/resource/System Settings/System Settings` works, but
  `GET /api/resource/System Settings` (no docname, which Frappe's list endpoint redirects to the
  single) and `GET /api/v2/document/System Settings` both return
  `DatabaseError "Internal Server Error"` (the single is routed down the *list* SQL path, which tries
  to `SELECT ... FROM "tabSystem Settings"` — a table that doesn't exist). This is an ORM
  list-routing nuance (not a meta-struct defect: `meta.issingle` is correct), but it's an observable
  divergence — Frappe never throws DatabaseError for a single. Worth a follow-up: in the list path
  (`src/orm.rs:get_list` and the v2 document GET), short-circuit `if meta.issingle { return
  get_single(...) }`.

- **U2 — `columns` is built from PRAGMA, so a stale/trimmed physical column is silently a valid
  query column.** Because ferro trusts the live table schema rather than recomputing from docfields,
  a column that exists in the DB but no longer has a docfield (Frappe's `trim_tables` target) is
  still accepted by ferro's guard, whereas Frappe's computed `_valid_columns` would reject it. This
  is the inverse face of P1/G1 and is generally *safe* (the column physically exists) but is a
  fidelity difference.

- **U3 — `MetaCache` is a global bounded LRU (cap 512 in `src/main.rs:401,661`, 2048 in
  `ferrod.rs:574`) with no TTL.** Frappe's meta cache is a per-request/site client cache invalidated
  on change. Combined with G3 (no invalidation), ferro's cache is correct only for an immutable
  schema during the process lifetime; this is by design for the footprint goal but is undocumented as
  a constraint.

- **U4 — DocField load coalesces `fieldtype` to `'Data'` and `fieldname` to empty (then drops empty
  rows).** `src/meta.rs:132,138,148`. Frappe doesn't silently default a NULL fieldtype to Data at
  meta time; harmless in practice (DocFields always have a fieldtype) but a quiet divergence.
