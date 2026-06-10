# db-api behavioral fidelity: Frappe `frappe.db.*` / `frappe.client.*` vs ferro

Spec: `frappe/tests/test_db.py` (class `TestDB`, `TestDBSetValue`, `TestConcurrency`, `TestSqlIterator`, ...).
ferro: `src/orm.rs` (the ORM) + `src/desk.rs` `route_method` (`frappe.client.get_value` / `get_single_value` /
`get_count` / `get_list`) + `src/main.rs` (`build_list_query`, v1 `/api/resource`, v2 `/api/v2/document`).

## Scope / reachability over HTTP

`frappe.db.*` is a **Python in-process API**. ferro is HTTP-only, so almost none of `test_db.py` runs against
ferro directly. What is reachable is the small subset of `frappe.db` behaviors that Frappe re-exposes through
**whitelisted methods** (`frappe.client.get_value`, `get_single_value`, `get_count`, `get_list`, `get`) and through
the REST resource planes (`/api/resource`, `/api/v2/document`). The remaining `test_db.py` assertions
(savepoints, transaction-write counting, callbacks, read-only mode, multi-statement guards, `bulk_insert`,
`bulk_update`, statement timeouts, replica switching, locking/`for_update`/`skip_locked`/`wait`, DDL,
`frappe.db.sql`, `estimate_count`, `get_database_size`, `as_iterator`/unbuffered cursors, datetime serialization)
are **pure in-process Python internals with no HTTP surface** — rubric **"shouldn't care"** for pure ferro; they
encode contracts that ferro's read/write methods must *approximate* but ferro is under no obligation to expose
the same primitives. I rate the in-scope obligation as **1:1** for the value-read/filter/escaping contract that
`frappe.client.get_value/get_count/get_list` re-expose, and **shouldn't-care** for the transaction/DDL/sql machinery.

Note: ferro's `frappe.client.*` methods are only active with the `--desk` flag (dispatch is gated on
`app.desk.is_some()`, `main.rs:1230`). The live server under test has them enabled.

## Behaviors Frappe guarantees (relevant subset)

- **get_value** (`frappe.client.get_value(doctype, fieldname, filters, ...)`, `client.py`): `fieldname` is the
  **2nd positional** arg and is `frappe.parse_json`'d — a JSON list `["name","email"]` returns **both** fields;
  a bare string returns one. `filters` as a **string** is coerced to `{"name": filters}`. Returns the first row;
  `{}` when nothing matches (as_dict default). For **Single** doctypes it routes to
  `get_values_from_single` (reads tabSingles, with fieldtype casting). `test_get_value`, `test_casted_get_value_singles`,
  `test_get_value_casts_singles`.
- **get_value filter operators** (`test_get_value`, `test_exists`): `=`, `!=`, `<`, `<=`, `>`, `>=`, `like`,
  `in`, `not in`, `between`, `is set/not set`.
- **get_single_value** (`frappe.db.get_single_value`, `test_get_single_value`, `test_casted_get_value_singles`):
  reads tabSingles `value`, then **casts by fieldtype** — Check/Int→`cint` (int), Currency/Float/Percent→`flt`,
  Data/Select/Link/…→`cstr` (`cstr(None)==""`), Date→`getdate`, Datetime→`get_datetime`, Time→`to_timedelta`.
  Round-trips every fieldtype exactly. Missing/None value → `cast(fieldtype, None)`, **not** literal `0`.
- **get_count** (`test_count`, `test_estimated_count`): COUNT honoring filters incl. `[["DocType","field","op","val"]]`
  4-tuple table-qualified filters and `[["field","op","val"]]`.
- **get_list / get_all** (`test_db_keywords_as_fields`, `test_get_list_return_value_data_type`): list of dicts;
  field aliasing (`` `field` as total `` → key `total`), aggregate field dicts (`{"COUNT": f}` →
  `COUNT(\`f\`)` key), `distinct`, `pluck`, `as_list`, grave-accent-quoted keyword field names.
- **filters as string `in`** splits a comma list; empty `in`→no rows, empty `not in`→all rows.
- **exists** (`test_exists`): `exists(dt, name)`→name; `exists(dt, {"name": (op, v)})`→name; `exists(dt, [["name","=",v]])`→name;
  `exists({"doctype": dt, ...})` form; returns the matched name (truthy) or None.
- **escaping/injection**: field/order_by identifiers validated against schema; values bound as parameters;
  grave-accent keyword fields quoted (`test_db_keywords_as_fields`).
- **NULL handling**: `is set` ⇒ `IS NOT NULL AND != ''`; `is not set` ⇒ `IS NULL OR = ''` (test_is, postgres-gated but
  the semantics are db-agnostic).
- **set_value** (`TestDBSetValue`): single-row, multi-column, multi-row, `update_modified`, custom `modified`/`modified_by`,
  None-name no-op, Single routing to `set_single_value`. **Writes — in-process; over ferro reachable only via
  `frappe.client.set_value` (NOT implemented) or PUT `/api/v2/document`.**

## MATCH (ferro replicates)

1. **Filter operators** `= != < <= > >= <> like not-like in not-in between is/is-not-set** — `orm.rs:148-211`
   (`op_clause`). Verified live: `like Admin%`→Administrator; `between` dates→count 68; `is set`→rows.
   (`test_get_value`, `test_exists`, `test_is`.)
2. **Filter shapes**: object `{"f": v}`, object `{"f": [op,v]}`, list `[[f,op,v]]`, **4-tuple**
   `[[DocType,f,op,v]]`, and 2-element `[f,v]` — `orm.rs:218-260` (`parse_filters`). (`test_count` uses the
   4-tuple table-qualified form; verified count=correct.)
3. **`in` string-splitting + empty-list semantics** — `orm.rs:140-146`, `171-192`: empty `in`→`0` (no rows),
   empty `not in`→`1` (all rows). (Matches db_query.)
4. **get_count honoring filters** — `desk.rs:800-817` → `orm::count` (`orm.rs:972`). Verified: `status=Open`→68,
   `status=Closed`→0, list-of-list filter→correct. (`test_count`.)
5. **Injection-safe identifiers**: unknown filter field → 417 ValidationError (`orm.rs:148-154`); unknown
   order_by field → 417 (`orm.rs:274`); values param-bound (`json_to_sql`, never interpolated). Verified:
   `{"name; DROP TABLE...":...}`→ValidationError; `status="Open' OR '1'='1"`→count 0; `order_by=name; DROP`→
   ValidationError. (`test_db_keywords_as_fields` escaping intent.)
6. **Identifier quoting** via `quote_ident` everywhere (`orm.rs:155,277,319,...`) — keyword/space-containing
   columns are grave-quoted, so DB-keyword docfield names work (`test_db_keywords_as_fields` read path for a
   plain field name).
7. **order_by with direction + multi-term + table-qualifier stripping** — `orm.rs:262-284` (`validate_order_by`)
   and `desk.rs:709-723`. Verified: `order_by=creation desc`→ordered. (`test_get_value` multi-orderby intent —
   ferro builds the equivalent ORDER BY, though it cannot echo SQL via `run=False`; see GAP-7.)
8. **get_value no-match → `{}`** — `desk.rs:1049` (`rows.into_iter().next().unwrap_or(json!({}))`). Verified.
   (`get_value` as_dict contract.)
9. **get_value default fieldname = name** — `build_query` always injects `name` (`desk.rs:693-695`); single
   field via `fieldname` arg honored (`desk.rs:1040-1045`). Verified `fieldname=name`→`{"name":...}`.
10. **list returns array-of-dicts** (`get_list`/`get_all` default `as_dict=True`) — `orm.rs:386-397` returns
    `Value::Object` rows. (`test_get_list_return_value_data_type` data-shape intent.)
11. **Single read casts Check/Int/Float in the document path** — `orm.rs:94-107` (`cast_by_fieldtype`) used by
    `get_single` (`orm.rs:496`). (Partial coverage of `test_casted_get_value_singles` — but NOT used by the
    `get_single_value` whitelisted method; see GAP-2.)

## PARTIAL

- **get_list field selection** — `desk.rs:688-698`: keeps only fields that are real columns, drops alias/computed,
  always prepends `name`. Plain field lists work; **aliases and aggregate-dict fields are silently degraded**
  (see GAP-3/GAP-4). Substantive read works; projection fidelity is partial.
- **Single-document read casting** — `get_single` casts Check/Int/Float/Currency/Percent but returns
  `Value::Null` for a None value of a Data field where Frappe returns `""` (`orm.rs:99`); Date/Datetime/Time are
  returned as raw text, not re-serialized (acceptable over JSON, but not byte-identical to a Python cast).

## GAP

### GAP-1 — `get_value` multi-field (`fieldname` as JSON list) returns only the first field — **Med**
Frappe `parse_json`'s `fieldname`; `["name","email"]` must return `{"name":..., "email":...}`. ferro's
`method_get_value` (`desk.rs:1040-1045`) treats `fieldname` as a single string via `bare_field`, so
`["name","email"]` becomes the bare token `email` (or is dropped), and `build_query` already collapsed
`fields` to `[name]`. Verified: `fieldname=["name","email"]`→`{"name":"Administrator"}` (email missing).
Test: `test_get_value` (`get_value("User","Administrator",["name","email"])`).
**Fix (`desk.rs:method_get_value`, ~1040)**: parse `fieldname` as JSON: if it parses to an array, map each
element through `bare_field`, retain those that `meta.has_column`, set `q.fields = that list` (still prepend
`name` only if requested? — Frappe does *not* auto-add name here, so set exactly the requested readable fields);
if it parses to a scalar/non-JSON, treat as one field. Then return the single row dict unchanged.

### GAP-2 — `get_single_value` does not cast by fieldtype and returns `0` for None — **Med**
Frappe casts the tabSingles value via `cast_fieldtype(df.fieldtype, val)`: Check/Int→int, Float/Currency/Percent→float,
Data→`cstr` (None→`""`). ferro's `method_get_single_value` (`desk.rs:1056-1076`) reads the raw `value` string and
returns it verbatim, and returns literal `json!(0)` when the row is missing/None. Verified: `enable_telemetry`
(Check)→`"1"` (string, should be int `1`); `language` (Data, None)→`0` (should be `""`).
Tests: `test_get_single_value` (round-trips every fieldtype), `test_casted_get_value_singles` (asserts `type==int`).
**Fix (`desk.rs:method_get_single_value`)**: load the meta (already have `metas`), look up the docfield, and run
the fetched value through the existing `cast_by_fieldtype` analogue (or a new `cast(fieldtype, Option<String>)`
helper in `orm.rs`/`util.rs`): Check/Int→`util::cint`, Float/Currency/Percent→`util::flt`, else `cstr` semantics
(None→`""`). Replace the `None → json!(0)` fallback with `cast(df.fieldtype, None)`.

### GAP-3 — `get_value` / `get_list` ignore Single doctypes (queries a nonexistent `tab<Single>` table) — **High**
Frappe `get_value`: if `meta.issingle`, route to `get_values_from_single`. ferro's `method_get_value` always
calls `orm::get_list`, which queries `tab<DocType>`. For a Single that physical table does not exist, so ferro
500s. Verified: `get_value(doctype="System Settings", fieldname="enable_telemetry")` →
`{"exc_type":"DatabaseError","message":"no such table: tabSystem Settings"}`.
Tests: `test_casted_get_value_singles`, `test_get_value_casts_singles` (`get_value("System Settings", None, ...)`).
**Fix (`desk.rs:method_get_value`, ~1035)**: after loading `meta`, if `meta.issingle` read from tabSingles for
each requested field (reuse the `orm::get_single` path / the `get_single_value` query per field), casting each by
fieldtype, and return the dict — mirroring `get_values_from_single`. `orm::get_list` already guards singles for
the resource plane only indirectly; add an explicit single branch here.

### GAP-4 — field aliasing (`` `f` as total ``) is stripped, alias key lost — **Low**
Frappe returns the aliased key (`total`). ferro's `bare_field` (`desk.rs:663-673`) drops the `as alias` and uses
the bare column, and `build_list_query` (`main.rs:1436`) / `orm::get_list` (`orm.rs:302-320`) select the column
under its own name. Verified: `fields=["name as foo"]`→`{"name":...}` (expected `{"foo":...}`).
Test: `test_db_keywords_as_fields` (`` `{field}` as total `` → key `"total"`).
**Fix (`orm.rs:get_list` select builder + `desk.rs:build_query`)**: parse `field as alias`, emit
`SELECT <quoted col> AS <quoted alias>` and key the result row by the alias. Validate the *column* part against
the schema (alias is free text), reject otherwise.

### GAP-5 — aggregate field dicts (`{"COUNT": f}`, `{"MAX": f}`) not supported — **Low**
Frappe accepts `fields=[{"COUNT": "name"}]` etc. and returns the function result keyed `COUNT(\`name\`)`
(or, for `get_value`, `[{"MAX": "name"}]`). ferro's `parse_fields_arg` (`desk.rs:675-686`) only keeps string
fields; objects are dropped and the query falls back to `name`. Verified: `fields=[{"COUNT":"name"}]`→
`{"name":...}`. Tests: `test_get_value` (`[{"MAX":"name"}]`, `[{"MIN":"name"}]`), `test_db_keywords_as_fields`
(`[{"COUNT": field}]`).
**Fix**: extend field parsing to recognize a `{FUNC: field}` object; whitelist FUNC ∈ {COUNT,MAX,MIN,SUM,AVG};
validate `field` against schema; emit `FUNC(<quoted col>)` with the matching alias and (for aggregates with no
group_by) suppress the default ORDER BY. Out of footprint scope to fully implement; document as known divergence.

### GAP-6 — `distinct` / `group_by` / `pluck` / `as_list` not honored — **Low**
`frappe.client.get_list` accepts `group_by`; `frappe.db.get_values` accepts `distinct`. ferro's `build_query`
(`desk.rs:688`) and `build_list_query` (`main.rs:1434`) parse none of `distinct`, `group_by`, `pluck`, `as_list`.
Verified: `distinct`/`pluck`/`as_list` params are ignored. Tests: `test_get_value` (`distinct=True` →
`SELECT DISTINCT email`), `test_db_keywords_as_fields` (`distinct=True`). Note `pluck`/`as_list`/`as_dict` are NOT
part of the `frappe.client.get_list` whitelisted signature (db.py-internal), so only `group_by` (and `distinct`
via `get_values`, which is itself in-process) is strictly HTTP-relevant — low severity.
**Fix (`orm::get_list`)**: thread a `distinct: bool` and `group_by: Option<String>` through `ListQuery`; emit
`SELECT DISTINCT` and `GROUP BY <validated cols>`. `pluck`/`as_list` are response-shape transforms ferro can apply
post-query if ever needed.

### GAP-7 — `run=False` (return SQL string instead of executing) not supported — **Low / shouldn't-care**
Many `test_get_value`/`test_set_value` assertions check the *generated SQL string* (`for_update` → `"for update"`,
`order_by` echoed, `concat_ws`). ferro builds different SQL (SQLite, no `FOR UPDATE`) and never returns query
text. This is an in-process introspection feature with no HTTP surface and is **not** something ferro should
replicate. Rubric: shouldn't-care. (Listed for completeness; not counted as a fixable contract gap below.)

### GAP-8 — `for_update` / `skip_locked` / `wait=False` (`TestConcurrency`) — **Low / shouldn't-care**
`get_value(..., for_update=True, skip_locked=True/wait=False)` row-locking semantics. SQLite has no row locks and
ferro serializes writes; these tests can't be expressed. Architectural; shouldn't-care for pure ferro.

### GAP-9 — no `frappe.client.set_value` / no in-process write API — **Low (write path covered elsewhere)**
`TestDBSetValue` exercises `frappe.db.set_value` (single-col, multi-col, multi-row, update_modified, custom
modified). ferro has no `frappe.client.set_value` method; the equivalent over HTTP is PUT `/api/v2/document`
(full-doc update, `main.rs:1167` → `orm::update`), which always stamps `modified`/`modified_by` and offers no
`update_modified=False`, no multi-row-by-filter update, no custom `modified` override. Writes through the test
harness are blocked by SQLite contention (per shared context); standalone writes work. Severity low for the read
domain; the multi-row-by-filter and `update_modified` controls are genuine missing capabilities.
**Fix**: add a `frappe.client.set_value` method in `desk.rs` mapping to a new `orm::set_value` that supports
`{filters | name}`, single field or dict of fields, optional `update_modified`/`modified`/`modified_by`, and
Single routing to `update_single`.

## UNDOCUMENTED ferro behavior (divergences not covered by a test)

- **`get_value` with `filters` as a bare string crashes or mis-resolves.** Frappe coerces a string `filters` to
  `{"name": filters}`. ferro: a JSON string `filters="Administrator"` → 417 `"filters must be a list or object"`
  (`orm.rs:257`); a non-JSON `filters=Administrator` silently parses to nothing and returns the *first arbitrary
  row* (verified: returned a random ToDo/User's email). Both are wrong and the second is a **silent
  wrong-row** divergence. Same root as GAP-1 (no string→`{"name":...}` coercion in `method_get_value`).
- **`get_single_value` missing-row sentinel is `0`, not the Frappe cast of None.** Even setting aside fieldtype
  casting, returning integer `0` for an absent single value (`desk.rs:1074`) is a ferro-specific sentinel that
  collides with a legitimately-`0` Check field and differs from Frappe's `""`/None/`0.0` per-type result.
- **`get_value` does not enforce `read` permission on the target doctype** the way `frappe.client.get_value`
  does (`has_permission` at `client.py`). ferro's `method_get_value`/`method_get_list`/`method_get_count` use
  `ReadAcl::all()` unconditionally (`desk.rs:760,792,1047`) — no permission gate and no permlevel masking on the
  desk method path (unlike the `/api/v2/document` path, which does gate via `auth::permission`). Combined with the
  documented Guest-role bug, the `frappe.client.*` data methods are effectively unauthenticated-read. Not covered
  by `test_db.py` but a real security divergence for this domain.
- **`get_list` always injects `name` into the field list** (`desk.rs:693-695`) even when the caller requested a
  different single field; Frappe returns exactly the requested fields. Harmless for most callers but a shape
  divergence (extra `name` key).
- **`get_list`/`get_value` default ORDER BY** falls back to `modified DESC` (or meta `sort_field`) when none is
  given (`orm.rs:280,356`). Frappe's `db_query` default is also `modified desc`, so this matches — but ferro
  applies it even to aggregate/`order_by=None` queries where Frappe drops ORDER BY, which would break the
  `[{"MAX":"name"}]` aggregate case (interacts with GAP-5).
