# Frappe spec extract: meta-merge

Excellent! Now let me compile the full picture. Let me create the final markdown report:

Based on my reading of `/home/frappe/benches/bench-cpython314/apps/frappe/frappe/model/meta.py` and `/home/frappe/benches/bench-cpython314/apps/frappe/frappe/model/__init__.py`, here is the precise specification:

---

# Frappe DocType Meta Augmentation Specification for Rust

## 1. add_custom_fields() — Custom Field Merge

**Source:** `frappe/model/meta.py:404–420`

### Query Pattern
```python
custom_fields = frappe.db.get_values(
    "Custom Field",
    filters={"dt": self.name},
    fieldname="*",
    as_dict=True,
    order_by="idx",
    update={"is_custom_field": 1},
)
```

### Columns Read from `tabCustom Field`
Custom Field table (`frappe/custom/doctype/custom_field/custom_field.json`) fields relevant to DocField:
- `dt` (Link to DocType) — used in filter
- `label` (Data, 255 chars)
- `fieldname` (Data, read-only)
- `insert_after` (Select) — critical for ordering
- `fieldtype` (Select, required) — e.g., "Data", "Link", "Table", etc.
- `options` (Small Text)
- `fetch_from` (Small Text)
- `fetch_if_empty` (Check, default 0)
- `default` (Text)
- `depends_on` (Code)
- `mandatory_depends_on` (Code)
- `read_only_depends_on` (Code)
- `reqd` (Check, default 0)
- `unique` (Check, default 0)
- `is_virtual` (Check, default 0)
- `read_only` (Check, default 0)
- `ignore_user_permissions` (Check, default 0)
- `hidden` (Check, default 0)
- `print_hide` (Check, default 0)
- `print_hide_if_no_value` (Check, default 0)
- `alignment` (Select)
- `no_copy` (Check, default 0)
- `allow_on_submit` (Check, default 0)
- `in_list_view` (Check, default 0)
- `in_standard_filter` (Check, default 0)
- `in_global_search` (Check, default 0)
- `in_preview` (Check, default 0)
- `bold` (Check, default 0)
- `report_hide` (Check, default 0)
- `search_index` (Check, default 0)
- `ignore_xss_filter` (Check, default 0)
- `translatable` (Check, default 0)
- `length` (Int)
- `precision` (Select: 0–9)
- `permlevel` (Int, default 0)
- `width` (Data)
- `columns` (Int)
- `collapsible` (Check, default 0)
- `collapsible_depends_on` (Code)
- `description` (Text)
- `placeholder` (Data)
- `button_color` (Select)
- `set_only_once` (Check, default 0)
- `hide_seconds` (Check, default 0)
- `hide_days` (Check, default 0)
- `hide_border` (Check, default 0)
- `non_negative` (Check, default 0)
- `sort_options` (Check, default 0)
- `link_filters` (JSON)
- `show_dashboard` (Check, default 0)

### Processing
1. **Query ordering:** Ordered by `idx` ascending (the `Custom Field.idx` column from the seed, auto-incremented when created)
2. **Marker addition:** `is_custom_field=1` is set on each row via the `update` parameter (this is metadata, not a stored column read from DB)
3. **Merge:** All rows are appended to `meta.fields` via `self.extend("fields", custom_fields)` — this is a simple list append; no insertion-position computation yet
4. **idx field:** Custom fields rows already have an `idx` field from the Custom Field table (integer, order-sensitive). This field is NOT directly used for position yet — it just maintains insertion order from the DB query.

### Key Contract
- Custom fields are **appended** to the fields list in DB order (`idx` ascending)
- Each row dict is assigned `is_custom_field=1` to mark it as custom (used later in `sort_fields()`)
- All other columns are left as-is; no type coercion at this stage

---

## 2. apply_property_setters() — Property Override

**Source:** `frappe/model/meta.py:422–462`

### Query Pattern
```python
property_setters = frappe.db.get_values(
    "Property Setter",
    filters={"doc_type": self.name},
    fieldname="*",
    as_dict=True,
)
```

### Columns Read from `tabProperty Setter`
- `doctype_or_field` (Select: "DocType", "DocField", "DocType Link", "DocType Action", "DocType State")
- `field_name` (Data) — only populated if `doctype_or_field == "DocField"`
- `row_name` (Data) — for "DocType Link" / "DocType Action" / "DocType State"
- `property` (Data, required) — the property name to override (e.g., "label", "hidden", "reqd", etc.)
- `property_type` (Data) — the Frappe fieldtype string (e.g., "Check", "Int", "Data")
- `value` (Small Text) — the new value (string)
- `doc_type` (Link) — filter key

### Type Coercion via `cast()`

**Source:** `frappe/utils/data.py` — the `cast(fieldtype, value)` function

```python
def cast(fieldtype, value=None):
	"""Cast the value to the Python native object of the Frappe fieldtype provided."""
	if fieldtype in ("Currency", "Float", "Percent"):
		value = flt(value)  # → float
	elif fieldtype in ("Int", "Check"):
		value = cint(sbool(value))  # → int (0 or 1 for Check; sbool("1") → True → 1)
	elif fieldtype in (
		"Data", "Text", "Small Text", "Long Text", "Text Editor",
		"Select", "Link", "Dynamic Link"
	):
		value = cstr(value)  # → str
	elif fieldtype == "Date":
		if value:
			value = getdate(value)  # → datetime.date
		else:
			value = datetime.datetime(1, 1, 1).date()
	elif fieldtype == "Datetime":
		if value:
			value = get_datetime(value)  # → datetime.datetime
		else:
			value = datetime.datetime(1, 1, 1)
	elif fieldtype == "Time":
		value = get_timedelta(value)  # → datetime.timedelta
	return value
```

**Helper functions:**
- `flt(value)` → float, handles None → 0.0
- `cint(value)` → int, handles None → 0
- `cstr(value)` → str, handles None → ""
- `sbool(value)` → bool, parses string "1"/"0" → True/False
- `getdate(value)` → datetime.date from string "YYYY-MM-DD"
- `get_datetime(value)` → datetime.datetime from ISO string
- `get_timedelta(value)` → datetime.timedelta

### Apply Logic

```python
for ps in property_setters:
	if ps.doctype_or_field == "DocType":
		# Apply to meta (DocType) itself
		self.set(ps.property, cast(ps.property_type, ps.value))

	elif ps.doctype_or_field == "DocField":
		# Apply to a field in meta.fields
		for d in self.fields:
			if d.fieldname == ps.field_name:
				d.set(ps.property, cast(ps.property_type, ps.value))
				break

	elif ps.doctype_or_field == "DocType Link":
		# Apply to a link in meta.links
		for d in self.links:
			if d.name == ps.row_name:
				d.set(ps.property, cast(ps.property_type, ps.value))
				break

	elif ps.doctype_or_field == "DocType Action":
		# Apply to an action in meta.actions
		for d in self.actions:
			if d.name == ps.row_name:
				d.set(ps.property, cast(ps.property_type, ps.value))
				break

	elif ps.doctype_or_field == "DocType State":
		# Apply to a state in meta.states
		for d in self.states:
			if d.name == ps.row_name:
				d.set(ps.property, cast(ps.property_type, ps.value))
				break
```

### Key Contract
- **Type coercion is MANDATORY**: `cast(ps.property_type, ps.value)` converts the string `value` from DB to the correct Python type
- Properties can be DocType-level (e.g., `hidden`, `allow_rename`, `allow_import`) or field-level (e.g., `label`, `read_only`)
- DocType Link/Action/State rows are identified by exact `name` match (a UUID-like identifier), not fieldname
- Field matching uses `fieldname` (exact string match); search terminates at first match

---

## 3. Standard/Default/Optional Fields Definitions

**Source:** `frappe/model/__init__.py:82–96`

### default_fields
```python
default_fields = (
	"doctype",
	"name",
	"owner",
	"creation",
	"modified",
	"modified_by",
	"docstatus",
	"idx",
)
```
**Note:** In `meta.py:132`, the Meta class strips the first element ("doctype"), so `Meta.default_fields = list(default_fields)[1:]` = `("name", "owner", "creation", "modified", "modified_by", "docstatus", "idx")`

**Applied to:** ALL DocTypes (both parent and child tables)

### child_table_fields
```python
child_table_fields = ("parent", "parentfield", "parenttype")
```
**Applied to:** ONLY child table DocTypes (doctypes with `istable == True`)

### optional_fields
```python
optional_fields = ("_user_tags", "_comments", "_assign", "_liked_by", "_seen")
```
**Applied to:** May appear in tables but are not required; typically only added for special/optional features

### Columns Used for Valid Column Computation

**Source:** `frappe/model/meta.py:260–277` (get_valid_columns)

```python
valid_columns = self.default_fields + [
	df.fieldname
	for df in self.get("fields")
	if not getattr(df, "is_virtual", False) and df.fieldtype in data_fieldtypes
]
if self.istable:
	valid_columns += list(child_table_fields)
```

**Logic:**
1. Start with `Meta.default_fields`: `("name", "owner", "creation", "modified", "modified_by", "docstatus", "idx")`
2. Add all non-virtual DocField entries whose fieldtype is in `data_fieldtypes` (excludes "Section Break", "Column Break", "HTML", etc.)
3. If doctype is a child table (`istable == True`), append `("parent", "parentfield", "parenttype")`

### Default Field Descriptions

**Source:** `frappe/model/meta.py:51–63` (DEFAULT_FIELD_LABELS)

```python
DEFAULT_FIELD_LABELS = {
	"name": _lt("ID"),
	"creation": _lt("Created On"),
	"docstatus": _lt("Document Status"),
	"idx": _lt("Index"),
	"modified": _lt("Last Updated On"),
	"modified_by": _lt("Last Updated By"),
	"owner": _lt("Created By"),
	"_user_tags": _lt("Tags"),
	"_liked_by": _lt("Liked By"),
	"_comments": _lt("Comments"),
	"_assign": _lt("Assigned To"),
}
```

---

## 4. Field Ordering via sort_fields()

**Source:** `frappe/model/meta.py:543–625`

### Priority Rules (descending)
1. **field_order property setter** — if a DocType has a Property Setter with `property="field_order"` and `property_type="JSON"`, parse it as JSON array and use that order (filters to only extant fields)
2. **insert_after for custom fields** — custom fields with `insert_after` set; resolved to nearest position in field_order
3. **Default standard field order** — standard (non-custom) fields maintain relative order; custom fields inserted via insert_after map

### idx Assignment

**Source:** `frappe/model/meta.py:617–625` (_update_fields_based_on_order)

```python
def _update_fields_based_on_order(self, field_order):
	sorted_fields = []
	for idx, fieldname in enumerate(field_order, 1):  # enumerate starts at 1
		field = self._fields[fieldname]
		field.idx = idx
		sorted_fields.append(field)
	self.fields = sorted_fields
```

**Key:** `idx` is **recomputed post-sort**, starting at 1 and incrementing by 1 for each position in `field_order`. The original `Custom Field.idx` from the DB is DISCARDED; what matters is the final position after sort_fields() runs.

### Custom Field insert_after Resolution

When a custom field has `insert_after` set:
- The field is inserted into the `field_order` array **after** the specified `insert_after` fieldname
- If `insert_after` is a Section Break or Column Break, special logic finds the next sibling break and inserts just before it (to keep breaks grouped)
- If `insert_after` fieldname doesn't exist in `field_order`, the field is appended to the end
- If `insert_after` is None, the custom field is prepended to the front

---

## Summary for Rust Implementation

### add_custom_fields()
- Query `tabCustom Field` filtered by `dt = doctype_name`, ordered by `idx`
- Read all columns (full row as dict)
- Set `is_custom_field: true` on each row (metadata flag)
- Append rows to `meta.fields` list (simple extend)

### apply_property_setters()
- Query `tabProperty Setter` filtered by `doc_type = doctype_name`
- For each row, check `doctype_or_field`:
  - "DocType" → set property on meta root via `cast(property_type, value)`
  - "DocField" → find field in `meta.fields` by `fieldname`, set property via `cast(property_type, value)`
  - "DocType Link" → find row in `meta.links` by `name`, set property
  - "DocType Action" → find row in `meta.actions` by `name`, set property
  - "DocType State" → find row in `meta.states` by `name`, set property
- **Type coercion:** Must implement `cast(fieldtype: str, value: str) → Value` per the mapping above

### sort_fields() and idx Computation
- Parse `meta.field_order` property (JSON array) if present; filter to extant fieldnames
- Build `field_order: Vec<&str>` as canonical order
- Iterate over `meta.fields`: for custom fields with `insert_after`, add to insertion_map; for standard fields, compute implicit insert_after or append
- Merge insertion_map into field_order
- **Recompute idx:** enumerate final field_order starting at 1, assign `field.idx = position`

### Default Fields (Rust constant lists)
```rust
const DEFAULT_FIELDS: &[&str] = &["name", "owner", "creation", "modified", "modified_by", "docstatus", "idx"];
const CHILD_TABLE_FIELDS: &[&str] = &["parent", "parentfield", "parenttype"];
const OPTIONAL_FIELDS: &[&str] = &["_user_tags", "_comments", "_assign", "_liked_by", "_seen"];
```
