# orm-filters behavioral fidelity: Frappe `test_db_query.py` vs ferro

Spec: `/home/frappe/benches/bench-cpython314/apps/frappe/frappe/tests/test_db_query.py` (1506 lines).
ferro: `src/orm.rs` (`get_list` / `parse_filters` / `op_clause` / `in_values` / `validate_order_by` / `count`),
`src/main.rs` (`build_list_query`, v2 list handler ~1112), `src/desk.rs` (`build_query`, `method_get_list`,
`method_reportview_get`, `method_get_count`).

Judgment rubric: **1:1** — list filters/operators/order/limit are the core ORM read contract ferro
explicitly reimplements and MUST match. Permlevel masking is also 1:1. Aggregates / SQL-function fields /
group_by / distinct / dotted (linked & child) fields / tree-walk operators are still in-scope but ferro's
footprint goals make full fidelity "somewhat" optional — they are scored as GAPs with severity weighted by
how common they are in real REST traffic.

Probes were run live against `http://127.0.0.1:8081` (ferro `serve bench-test --desk`) with a freshly
provisioned Administrator token. (The test harness rotates the DB/keys frequently, so a few probes were
re-provisioned; all cited results are from successful authenticated responses.)

---

## Behaviors Frappe guarantees (from the spec)

1. `{"name": "DocType"}` is in `DocType` list (basic) — `test_basic`, `test_fields`, `test_filters_4`.
2. Filter shape A — list of `[doctype, field, op, value]`: `[["DocType","name","like","J%"]]` — `test_filters_1`.
3. Filter shape B — dict-with-op: `{"name": ["like", "J%"]}` — `test_filters_2/3`.
4. Filter shape C — bare dict equality: `{"name": "DocField"}` — `test_filters_4`.
5. Filter shape D — 3-tuple `[field, op, value]` lists; and 4-tuple where the **first element may be a
   CHILD doctype** so the filter joins the child table (`[["DocField","fieldtype","=","Data"]]` filters
   `DocType` by its child `DocField`) — `test_set_field_tables`, `test_get_count` (child-table filter).
6. Operators: `=`, `!=`, `like`, `not like`, `in`, `not in`, `between`, `>`, `<`, `>=`, `<=`,
   `is set` / `is not set` (`["is","set"]` / `["is","not set"]`), `descendants of`, `ancestors of`,
   `not descendants of`, `not ancestors of` — many tests.
7. `in`/`not in` value forms: comma-separated string (`"DocType,DocField"`), **JSON-encoded list string**
   (`'["DocType","DocField"]'`), and a real list. JSON form must NOT comma-split values that contain commas
   (`'["Test, With Comma"]'`) — `test_in_not_in_filters`, `test_in_filter_json_encoded_values`.
8. `in None` ⇒ empty result; `not in None` ⇒ returns everything — `test_in_not_in_filters`.
9. Empty list/tuple: `in []` ⇒ `1=0` (no rows); `not in []` ⇒ `1=1` (all rows) — `test_coalesce_with_in_ops`.
10. `between [from,to]` filters the range; `between [from]` (single) ⇒ `from .. today`; `between None`
    ⇒ `today .. today` (current datetime); single-day datetime spans `00:00:00.000000 .. 23:59:59.999999`
    — `test_between_filters`, `test_between_filters_date_bounds`.
11. `is not set` must match BOTH NULL and `''` (empty string) — `test_db_filter_not_set`, `test_is_set_is_not_set`.
12. `None` equality (`{"field": None}`) ⇒ `field IS NULL` (not `=''`, no `ifnull`) — `test_none_filter`,
    `test_ifnull_none`. For the primary key (`name`) and other non-nullable cols, IFNULL is NOT wrapped.
13. `or_filters` is OR-combined and ANDed with `filters`. Accepts **list-of-dicts**
    (`[{"fieldtype":"Table"},{"fieldtype":"Select"}]`) AND list-of-lists (`[["DocType","istable","=",1]]`)
    — `test_or_filters`, `test_filter_sanitizer`.
14. `fields`: bare names; `*` expansion; aggregate/SQL-function fields (`count(\`name\`) as count`,
    `date(creation) as creation`, `locate(...)`, `SUM`/`COUNT` dict form `{"COUNT": "*"}`); `as` aliasing;
    **dotted child fields** (`child.title as child_title`, `seen_by.user as seen_by`) and **dotted link
    fields** (`allocated_to.email as ...`) — `test_child_table_field_syntax`, `test_link_field_syntax`,
    `test_child_table_join`, `test_function_alias_in_clauses`, `test_select_star_expansion`,
    `test_string_as_field`.
15. `order_by`: `name asc`/`desc`, multi-term, default `modified DESC`; blacklisted/subquery order_by ⇒
    `ValidationError` — `test_order_by_group_by_sanitizer`, `test_prepare_select_args`.
16. `group_by` + aggregate select — `test_function_alias_in_clauses`, `test_prepare_select_args`,
    `test_reportview_get_aggregation`. Blacklisted group_by ⇒ `ValidationError`.
17. `distinct` (reportview `get_count` distinct true/false) — `TestReportView.test_get_count`.
18. `limit_start` / `limit_page_length`; `limit_page_length=None/0` ⇒ unlimited — `test_basic`, `test_fields`.
19. `pluck="name"` / `pluck="owner"` ⇒ flat list of that column — `test_pluck_name`, `test_pluck_any_field`.
20. `as_list=True` ⇒ rows as arrays not dicts; `fields="name"` (string) == default — `test_string_as_field`,
    `test_prepare_select_args`.
21. SQL-injection sanitizer: malicious `fields`/`filters`/`order_by`/`group_by` raise `DataError` /
    `ValidationError`; benign keyword-containing strings allowed — `test_query_fields_sanitizer`,
    `test_filter_sanitizer`, `test_order_by_group_by_sanitizer`.
22. Permlevel field masking: querying/filtering/ordering on a higher-permlevel field as a non-privileged
    user raises `PermissionError`; in reportview, inaccessible fields are silently dropped from `keys`
    (even when `*`) — `test_permlevel_fields`, `test_reportview_get*`.
23. Permission scoping in list: user-permission restricts visible rows (match conditions); nested/tree
    user-permission lets descendants through; `if_owner` scopes to owner — `test_nested_permission`,
    `test_build_match_conditions`, `test_permission_query_condition`.
24. Field-to-field comparison: `{"creation": Field("modified")}` compares two columns — `test_field_comparison`.
25. Virtual doctype list goes to the controller's `get_list` (no table query) — `test_virtual_doctype`,
    `test_virtual_field_get_list`.

---

## MATCH (ferro replicates)

- **Filter shape C (bare dict eq)** `{"name":"DocField"}` — `parse_filters` Object branch falls through to
  `op_clause(...,"=",v)` (`orm.rs:245-255`). Probe returned `[{"name":"DocField"}]`. (`test_filters_4`)
- **Filter shape B (dict-with-op)** `{"name":["like","J%"]}` — Object branch, 2-elem array ⇒ op+val
  (`orm.rs:247-252`). Probe: empty result for `J%`. (`test_filters_2/3`)
- **Filter shape A/D (list 3-tuple, 4-tuple-same-doctype)** `[["DocType","name","like","J%"]]` and
  `[["name","like","Doc%"]]` — Array branch (`orm.rs:222-243`) handles arity 2/3/4. Probes returned the
  expected rows. (`test_filters_1`, `test_fieldname_starting_with_int`)
- **Operators `=` `!=` `>` `<` `>=` `<=` `<>`** — `op_clause` match arm `orm.rs:159-162`. Probe `creation > "2000-01-01"`
  returned rows. (`test_coalesce_with_datetime_ops` shape)
- **`like` / `not like`** — `orm.rs:163-170`. Probe: `not like "Doc%"` excluded Doc* names. (`test_fieldname_starting_with_int`)
- **`in` / `not in` with comma string & real list** — `in_values` (`orm.rs:140-146`) splits comma strings,
  passes arrays through. Probe `in "DocType,DocField"` returned both. (`test_in_not_in_filters`)
- **`in None` ⇒ empty** — `in_values(null)` ⇒ `vec![null]` ⇒ `name IN (NULL)` matches nothing. Probe: empty.
  (`test_in_not_in_filters`; correct, though by SQL-NULL coincidence rather than Frappe's `1=0`.)
- **`in []` ⇒ no rows** — empty array ⇒ returns clause `"0"` (`orm.rs:173-175`). Matches Frappe's `1=0`.
  (`test_coalesce_with_in_ops`)
- **`not in []` ⇒ all rows** — empty array ⇒ clause `"1"` (`orm.rs:183-186`). Matches Frappe's `1=1`. (`test_coalesce_with_in_ops`)
- **`between [a,b]`** — `orm.rs:193-201` binds both bounds. Probe `creation between [2000,2099]` returned rows;
  `between [2016-07-06,2016-07-07]` returned empty (no rows in range). (`test_between_filters`)
- **`is set` / `is not set`** — `orm.rs:202-209` emits `(col IS NOT NULL AND col != '')` /
  `(col IS NULL OR col = '')`. Correctly handles BOTH NULL and `''`. Probe: `autoname is "not set"` ⇒ User
  present, Blogger absent; `is "set"` ⇒ DocField present. (`test_is_set_is_not_set`, `test_db_filter_not_set`)
- **`or_filters` list-of-lists** `[["DocType","istable","=",1]]` AND-combined with `filters` — `get_list`
  builds `WHERE and_sql AND (or_sql)` (`orm.rs:339-352`). Probe with `filters={editable_grid:1,module:Core}` +
  this or_filter returned DocField. (`test_filter_sanitizer`)
- **`*` field expansion (permlevel-aware)** — `orm.rs:305-311` expands `*` to all readable columns. (`test_fields`)
- **`order_by name asc`, multi-term, default modified DESC** — `validate_order_by` (`orm.rs:262-284`),
  default branch `orm.rs:354-361`. Probe `order_by=name asc` returned alphabetical. (`test_order_by_group_by_sanitizer` allowed case)
- **`limit_page_length` default 20; `0` ⇒ unlimited** — `build_list_query` default `ListQuery::default()`
  (page_length 20, `orm.rs:128`); `get_list` `<=0 ⇒ LIMIT -1` (`orm.rs:363-369`). Probe: omitted ⇒ 20 rows,
  `=0` ⇒ 278. (`test_basic`, `test_fields`)
- **`limit_start` offset** — bound at `orm.rs:382`; aliases `start` accepted (`main.rs:1462`). (general)
- **Unknown filter field ⇒ error** — `op_clause` guards `meta.has_column` (`orm.rs:149-153`); ferro returns
  417 ValidationError. Frappe raises (different code) but the substantive "reject unknown column" behavior
  matches. (related to `test_filter_sanitizer`)
- **Permlevel field masking in lists** — `ReadAcl.can_read` (`orm.rs:37-45`); `get_list` drops unreadable
  fields silently (`orm.rs:316-318`) and rejects filter/order_by on masked fields (`orm.rs:149`, `:274`).
  This matches the reportview "silently drop inaccessible keys" behavior; partial vs the PermissionError-raise
  path — see PARTIAL. (`test_reportview_get*`)
- **`if_owner` row scoping** — `owner_scope` ANDs `owner = ?` (`orm.rs:327-332`, wired `main.rs:1130`). (`test_nested_permission` if_owner branch)

---

## PARTIAL

- **`or_filters` list-of-DICTS** `[{"fieldtype":"Table"},{"fieldtype":"Select"}]` (the canonical
  `test_or_filters` form) — **FAILS**. `parse_filters` Array branch requires each element be an array
  (`orm.rs:224-226` → "each filter must be a list"); a dict element errors. List-of-lists works, but the
  dict-list form (which `test_or_filters` and a lot of frappe-ui traffic use) 417s. Probe confirmed:
  `{"exc_type":"ValidationError","message":"each filter must be a list"}`. Scored PARTIAL because one of the
  two accepted or_filters shapes works. Fix below (GAP-1).
- **`is` operator value parsing** — ferro only honors literal `"set"`/anything-else-as-not-set
  (`orm.rs:203-208`: `unwrap_or("set")`, then `if want=="set"`). Frappe accepts `"set"`/`"not set"`. ferro
  treats any non-`"set"` value (incl typos) as not-set; Frappe would too for `"not set"` but a wrong value
  diverges. Behaviorally close enough for the tested values (`"set"`, `"not set"`). PARTIAL/Low.
- **Permlevel masking severity mismatch** — Frappe **raises `PermissionError`** when a list/get_list filters
  or selects an explicit higher-permlevel field (`test_permlevel_fields`). ferro instead: rejects the field
  on `filters`/`order_by` (417 ValidationError, not 403), and **silently drops** it from `fields`
  (`orm.rs:316-318`). For reportview's "drop from keys" path this matches; for the get_list explicit-field
  path the error class differs (417 vs 403). PARTIAL.
- **`count` ignores `or_filters` and ACL** — `orm::count` (`orm.rs:971-988`) parses only `filters` with
  `ReadAcl::all()`, no or_filters, no permlevel, no child-table join. `method_get_count` (`desk.rs:800-816`)
  also ignores `distinct`. So `get_count` with distinct/child filters (`TestReportView.test_get_count`)
  diverges. PARTIAL.

---

## GAP

### GAP-1 — `or_filters` (and any filter array) of DICTS not accepted — **Med**
`test_or_filters` passes `or_filters=[{"fieldtype":"Table"},{"fieldtype":"Select"}]`. The same dict-in-list
shape is also valid for `filters`. ferro's `parse_filters` Array branch (`orm.rs:222-243`) assumes every
array element is itself an array and errors otherwise.
**Fix location:** `src/orm.rs:222` (Array arm of `parse_filters`).
**Sketch:** in the `Value::Array` arm, if an element `cond` is a `Value::Object`, recurse into the Object
branch logic for that one dict (each key ⇒ an `op_clause`, AND-combined within that dict). i.e.
```rust
Value::Array(arr) => for cond in arr {
    if let Some(obj) = cond.as_object() {            // dict element
        for (field, v) in obj {
            let (op, val) = split_op(v);             // ["op",val] or bare ⇒ "="
            clauses.push(op_clause(meta, acl, field, op, &val)?);
        }
        continue;
    }
    // ...existing list-element handling...
}
```
(Factor the dict-element logic shared with the Object branch.)

### GAP-2 — `not in None` returns empty instead of all rows — **Med**
`test_in_not_in_filters`: `{"name":["not in", None]}` must return everything (DocType present). ferro:
`in_values(null)` ⇒ `[null]` ⇒ `name NOT IN (NULL)`; SQL `NOT IN (NULL)` is UNKNOWN ⇒ **no rows**. Probe
confirmed count 0. (Symmetric: `in None` happens to be correct because `IN (NULL)` is also empty.)
**Fix location:** `src/orm.rs:171-192` (`in` / `not in` arms) and/or `in_values` `orm.rs:140-146`.
**Sketch:** treat a `null` (or null-only) value specially: `in null` ⇒ `"0"` (no rows); `not in null` ⇒
`"1"` (all rows) — mirroring Frappe's empty-collection semantics. Also drop/normalize `null` elements out of
the value list before building the placeholder list.

### GAP-3 — `in` / `not in` JSON-encoded list string not parsed — **Med**
`test_in_filter_json_encoded_values`: `{"name":["in", '["DocType","DocField"]']}` must equal the
comma-separated form; and `'["Test, With Comma"]'` must NOT comma-split. ferro's `in_values` (`orm.rs:143`)
naively `split(',')` a string, so the JSON form becomes `['["DocType"', ' "DocField"]']` — neither matches.
Probe confirmed empty result.
**Fix location:** `src/orm.rs:140-146` (`in_values`).
**Sketch:** before splitting, try `serde_json::from_str::<Value>(s)`; if it parses to an `Array`, use those
elements verbatim (preserving embedded commas); otherwise fall back to comma-split. (Frappe does exactly this
JSON-decode-then-split in `db_query`.)

### GAP-4 — dotted child/link fields in `fields` rejected — **Med/High**
`test_child_table_field_syntax` (`seen_by.user as seen_by`), `test_link_field_syntax`
(`allocated_to.email as ...`), `test_child_table_join` (`child.title as child_title`). ferro's field
normalizer does `f.rsplit('.').next()` (`orm.rs:303-304`) which for `"seen_by.user as seen_by"` yields
`"user as seen_by"` — not a column ⇒ 417. Probe: `Unknown field 'user as seen_by' for Note`. ferro never
joins child tables or resolves link dotted fields in lists.
**Fix location:** `src/orm.rs:300-320` (field parsing/select build in `get_list`), plus FROM/JOIN assembly
`orm.rs:371-378`.
**Sketch:** parse each field into `(qualifier, column, alias)`. If `qualifier` is a child-table fieldname of
this doctype ⇒ LEFT JOIN `tab<Child>` on parent and SELECT `child.col AS alias`. If `qualifier` is a Link
field ⇒ LEFT JOIN the linked doctype on `<linkfield> = tab<Linked>.name` and SELECT `linked.col AS alias`.
At minimum, strip a trailing `as <alias>` and apply the alias before the `rsplit('.')` so aliases stop
breaking validation. (See also GAP-6 for the `as` alias on non-dotted fields, which is the cheap subset.)

### GAP-5 — aggregate / SQL-function fields rejected — **Med**
`count(\`name\`) as count`, `date(creation) as creation`, `locate(...)`, `{"COUNT":"*"}`/`{"SUM":1}` dict
fields, `_aggregate_column` — all required by `test_query_fields_sanitizer` (the *allowed* cases at the end),
`test_function_alias_in_clauses`, `test_select_star_expansion`, `test_reportview_get_aggregation`,
`test_prepare_select_args`. ferro validates each field as a literal column (`orm.rs:313-314`) ⇒ any function
expression 417s. Probe: `count(\`name\`) as count` ⇒ `Unknown field`. Also the dict field form
(`{"COUNT":"*"}`) is silently dropped by `build_list_query` (`main.rs:1437-1441` keeps only string fields).
**Fix location:** `src/orm.rs:300-320` (field validation), `src/main.rs:1436-1446` & `desk.rs:691`
(`parse_fields_arg` / dict-field handling).
**Sketch:** whitelist a small set of SQL functions (`count`, `sum`, `avg`, `min`, `max`, `date`, `locate`,
`ifnull`, `coalesce`, `datediff`) with a strict argument grammar (only bareword/quoted columns + literals),
emit them verbatim into SELECT with the parsed `as` alias; reject anything else (preserving the injection
guard). Map `{"COUNT":"*"}`/`{"SUM":x}` dict fields to `COUNT(*)`/`SUM(x)`. This both passes the
allowed-function tests and keeps the DataError on injection (`test_query_fields_sanitizer`).

### GAP-6 — `as` alias on plain fields rejected — **Med**
Even without a dot, `name as foo` 417s (`orm.rs:303-304` keeps the whole `"name as foo"` token). `test_string_as_field`
and many reportview tests rely on aliasing. Probe: `Unknown field 'name as foo' for DocType`.
**Fix location:** `src/orm.rs:301-320`.
**Sketch:** split each field on case-insensitive ` as ` first into `(expr, alias)`, validate/quote `expr`,
emit `quoted_expr AS quoted_alias`. (Cheap precursor that also unblocks GAP-4/5 alias handling.)

### GAP-7 — tree operators `descendants of` / `ancestors of` (and `not …`) unsupported — **Low/Med**
`test_of_not_of_descendant_ancestors` exercises all four on a tree (`is_tree`) doctype using `lft`/`rgt`
nested-set bounds. ferro's `op_clause` has no arm ⇒ `Unsupported operator 'descendants of'` (probe confirmed).
**Fix location:** `src/orm.rs:148-211` (`op_clause`), needs `lft`/`rgt` from meta.
**Sketch:** for `descendants of X`: subquery `WHERE lft > (SELECT lft FROM tab WHERE name=?) AND rgt <
(SELECT rgt FROM ...)`; `ancestors of X`: `lft < x.lft AND rgt > x.rgt`; `not …` negate. Requires the doctype
to have nested-set columns (tree doctypes). Low priority unless tree doctypes are served.

### GAP-8 — `between` single value / `between None` not handled — **Low**
`test_between_filters`: `between [from]` ⇒ `from .. today`; `between None` ⇒ `today .. today`. ferro requires
exactly 2 elements (`orm.rs:194-197`) ⇒ "between needs [a, b]" otherwise. Probes confirmed both error.
Also ferro does not apply Frappe's single-day datetime expansion to `23:59:59.999999`
(`test_between_filters_date_bounds`) — but those bounds are computed in `frappe.model.db_query`'s
`get_between_date_filter`, an in-process helper ferro can't be expected to byte-match.
**Fix location:** `src/orm.rs:193-201`.
**Sketch:** if value is `null` ⇒ `[today, today]`; if 1-element ⇒ `[v[0], today]`; for datetime fields with a
single date, expand `lo→ lo 00:00:00.000000`, `hi→ hi 23:59:59.999999`.

### GAP-9 — `group_by` query param silently ignored — **Med**
`ListQuery` has no `group_by` field; `build_list_query` (`main.rs:1434-1474`) and `desk.rs::build_query`
(`desk.rs:688-737`) never read `group_by`. Probe: `group_by=module` returned ungrouped duplicate rows (5×
"Core"). Required by `test_function_alias_in_clauses`, `test_prepare_select_args`,
`test_reportview_get_aggregation`. Silent ignore is worse than erroring (wrong results).
**Fix location:** add `group_by: Option<String>` to `ListQuery` (`orm.rs:110-130`); parse in both
`main.rs:1434` and `desk.rs:688`; validate (same blacklist as order_by) and emit `GROUP BY` in `get_list`
(`orm.rs:371-378`). Pairs with GAP-5 (group_by is only useful with aggregate selects). Also enforce the
blacklist so `group_by="SLEEP(0)"` raises (`test_order_by_group_by_sanitizer`).

### GAP-10 — `distinct` not supported — **Low**
No `distinct` flag in `ListQuery`/`build_list_query`/`build_query`; reportview `get_count`
(`desk.rs:800-816`) ignores `distinct`. `TestReportView.test_get_count` compares distinct vs non-distinct
counts. Low (mostly a reportview concern).
**Fix location:** `ListQuery` + `get_list` SELECT (`orm.rs:371`) emit `SELECT DISTINCT`; `count` emit
`COUNT(DISTINCT name)` when distinct (`orm.rs:985`).

### GAP-11 — `pluck` not supported — **Med**
`test_pluck_name`/`test_pluck_any_field`: `pluck="name"` ⇒ `["DocType"]` (flat list of scalars, not dicts).
ferro `get_list` always returns row objects; no `pluck` param anywhere. Commonly used in `frappe.get_all(...,
pluck=...)` and in REST list calls.
**Fix location:** `ListQuery` add `pluck: Option<String>`; `build_list_query`/`build_query` parse it; in
`get_list` (`orm.rs:393-397`) if `pluck` set, project each row to `row[pluck]` into a flat `Value::Array`.

### GAP-12 — `as_list` not supported on v1/v2 list path — **Low/Med**
`test_string_as_field`, `test_prepare_select_args`, `test_select_star_expansion` use `as_list=True` ⇒ rows as
arrays. ferro always returns dict rows on `/api/resource` and `frappe.client.get_list`. (Note:
`frappe.desk.reportview.get` DOES return the array-of-arrays `{keys,values}` form — `desk.rs:748-779` — which
covers the reportview tests, but not the generic `as_list` flag.)
**Fix location:** `ListQuery` add `as_list: bool`; in `get_list`, when set, emit arrays in `fields` order.

### GAP-13 — child-table (cross-doctype) filter not joined — **Med**
`test_set_field_tables` / `TestReportView.test_get_count` filter a parent by a CHILD doctype:
`[["DocField","fieldtype","=","Data"]]` on `DocType`. ferro's 4-tuple parser (`orm.rs:234-239`) IGNORES the
child doctype and treats `c[1]` ("fieldtype") as a column of the *current* doctype ⇒ since DocType has no
`fieldtype` column ⇒ 417 (and even if the name collided, it would query the wrong table). No JOIN to
`tabDocField` is generated.
**Fix location:** `src/orm.rs:222-243` (Array arm 4-tuple) + FROM/JOIN assembly `orm.rs:371-378`.
**Sketch:** when the 4-tuple's first element is a *different* doctype that is a child table of this doctype,
LEFT JOIN `tab<Child>` on `(parent = base.name AND parenttype = '<base>')` and apply the condition on the
child column. (Frappe's `_in_standard_sql_methods` / `extract_tables`.)

### GAP-14 — field-to-field comparison filter unsupported — **Low**
`test_field_comparison`: `{"creation": Field("modified")}` compares two columns; `("!=", Field("modified"))`.
The `Field(...)` marker arrives over REST as a magic string/object Frappe interprets. ferro binds the value
as a literal (`json_to_sql`), so it would compare against the literal string, not the column. Rarely used
over REST. Low.
**Fix location:** `src/orm.rs:148-211` — detect a `{"_field": "..."}`/`Field(...)`-encoded value and emit
`col OP other_col` (validated, no bind).

### GAP-15 — virtual doctype list returns `[]` (no controller dispatch) — **Low** (intentional)
`test_virtual_doctype`/`test_virtual_field_get_list` route a virtual-doctype list to the Python controller's
`get_list`. ferro `get_list` short-circuits virtual doctypes to `Ok([])` (`orm.rs:296-298`). No Python ⇒
can't dispatch; documented design limitation (pure-Rust has no controller). N/A for the deliverable;
`ferrod` python fallthrough is the answer.

---

## Undocumented ferro behaviors / divergences not covered by a test

- **`in null` returns empty for the right reason by accident.** `in_values(null)` produces a single `NULL`
  bind ⇒ `col IN (NULL)`, which is empty. Frappe explicitly emits `1=0`. Same observable result, different
  SQL; would diverge if combined with `or` (`col IN (NULL) OR …` vs `1=0 OR …` are equivalent, so harmless,
  but worth noting). (`orm.rs:140-146`)
- **`is` operator default-to-set on garbage value.** Any `is` value that isn't the string `"set"` is treated
  as "not set" (`orm.rs:203-208`), including a typo like `"st"`. Frappe would only special-case `"set"`/`"not
  set"`. Silent wrong-result for malformed input.
- **`>= / <= / <>` accepted but `<>` is a documented Frappe alias** — ferro passes `op` through verbatim into
  SQL for these (`orm.rs:159-162` formats `{col} {op} ?`), so `<>` works in SQLite. Fine, just undocumented
  that the raw operator is interpolated (it's gated to the whitelist arm, so no injection).
- **Filter validation error class is 417 ValidationError, not Frappe's `DataError`/`PermissionError`.** For
  malicious/unknown filter fields ferro returns 417 (`orm.rs:149-153`), whereas Frappe raises `DataError`
  (sanitizer) or `PermissionError` (permlevel). Clients keying on the exact `exc_type` will see a different
  code even though the request is correctly rejected.
- **`build_list_query` drops dict-form fields silently.** `main.rs:1437-1441` keeps only string fields; a
  `{"COUNT":"*"}` field vanishes with no error, so a count-only list call returns `name` rows instead. Silent
  semantic change.
- **No `LIMIT` cap / max page length.** ferro honors `limit_page_length=0` as truly unlimited (full table
  scan, 278 rows in probe). Frappe also allows this but some deployments cap it; ferro has no cap.
- **`order_by` strips link/child qualifiers down to the bare column.** `validate_order_by` does
  `field.rsplit('.').next()` and `trim_matches('`')` (`orm.rs:271`), so `\`tabToDo\`.\`modified\` desc`
  becomes `modified DESC` against the base table — correct for own columns, but an order_by on a *joined*
  child/link column would silently order by a (possibly non-existent) base column or 417. Untested edge.
- **Default order for a doctype follows its `sort_field`/`sort_order` meta**, not a hardcoded `modified DESC`,
  when `order_by` is omitted (`orm.rs:354-361`). Matches Frappe; just not asserted by these tests (DocType's
  sort_field is `modified` so the probe showed modified-desc, which looked unsorted-by-name — expected).
