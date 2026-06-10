# Frappe spec extract: client-methods

Now I have the complete source. Let me create a precise specification document for each method:

## Frappe `client.py` — Rust Reimplementation Behavior Spec

### `get(doctype, name, filters, parent) → dict`

**Signature:**
```python
def get(doctype: str, name: str | int | None = None, filters: str | list | dict[str, Any] | None = None, parent: str | None = None)
```

**Logic:**
1. **Name resolution** (exactly one path):
   - If `name` is truthy: `doc = frappe.get_doc(doctype, name)`
   - Elif `filters` is not None (including empty dict `{}`): `doc = frappe.get_doc(doctype, frappe.parse_json(filters))`
     - String filters are coerced to dict via `parse_json`
   - Else: `doc = frappe.get_doc(doctype)` (single doctype load)

2. **Permission checks** (ALWAYS):
   - `doc.check_permission()` — fails if no read permission
   - `doc.apply_fieldlevel_read_permissions()` — strips fields user cannot read

3. **Return:** `doc.as_dict(no_nulls=True)`
   - **Null stripping:** null/None values are REMOVED from output dict (no_nulls=True)
   - Return type: `dict[str, Any]`

---

### `get_value(doctype, fieldname, filters, as_dict, debug, parent) → scalar | dict | list | None`

**Signature:**
```python
def get_value(doctype: str, fieldname: str | list[str] | dict[str, Any], filters: str | list | dict[str, Any] | None = None, as_dict: int | bool = True, debug: int | bool = False, parent: str | None = None)
```

**Logic:**

1. **Permission check** (EARLY, before any DB access):
   ```python
   if not frappe.has_permission(doctype, parent_doctype=parent):
       raise PermissionError
   ```

2. **Filter normalization:**
   - `filters = get_safe_filters(filters)` — sanitizes
   - If result is a string: coerce to `{"name": filters}`
   - If no filters remain: `filters = None`

3. **Field parsing:**
   - Try `frappe.parse_json(fieldname)` → if succeeds and is JSON, use parsed result
   - Except (TypeError, ValueError): treat as single field name, wrap in list: `[fieldname]`
   - Result: `fields` is always a list

4. **Query path** (depends on doctype):
   - If doctype is **single**: `frappe.db.get_values_from_single(fields, filters, doctype, as_dict=as_dict, debug=debug)`
   - Else: call `get_list(doctype, filters=filters, fields=fields, debug=debug, limit_page_length=1, parent=parent, as_dict=as_dict)`

5. **Return shape** (depends on `as_dict` and result):
   - If `as_dict=True`:
     - If result non-empty: return `result[0]` (first dict)
     - Else: return `{}` (empty dict)
   - If `as_dict=False`:
     - If result empty: return `None`
     - Elif `len(fields) > 1`: return `result[0]` (list of values for multi-field)
     - Else: return `result[0][0]` (scalar, single value)

**Example return shapes:**
- `fieldname="name", filters={"id": 5}, as_dict=False, single_field` → scalar (e.g., `"DOC-001"`)
- `fieldname=["name", "title"], filters={"id": 5}, as_dict=False, multi_field` → list (e.g., `["DOC-001", "My Doc"]`)
- `fieldname="name", filters={"id": 5}, as_dict=True` → dict (e.g., `{"name": "DOC-001"}`)
- `fieldname=["name", "title"], filters={"id": 5}, as_dict=True` → dict (e.g., `{"name": "DOC-001", "title": "My Doc"}`)
- No matches + `as_dict=True` → `{}`
- No matches + `as_dict=False` → `None`

---

### `get_single_value(doctype, field) → Any | None`

**Signature:**
```python
def get_single_value(doctype: str, field: str)
```

**Logic:**
1. **Permission check** (EARLY):
   ```python
   if not frappe.has_permission(doctype):
       raise PermissionError
   ```

2. **Return:** `frappe.db.get_single_value(doctype, field)`
   - **Important:** Returns `None` if field is None/empty, NOT a default value like 0
   - Return type: whatever the field's value is, or `None`

---

### `get_list(doctype, fields, filters, group_by, order_by, limit_start, limit_page_length, parent, debug, as_dict, or_filters, expand) → list[dict] | list[list]`

**Signature:**
```python
def get_list(doctype: str, fields: str | list[str | dict[str, Any]] | None = None, filters: str | list | dict[str, Any] | None = None, group_by: str | list[str] | None = None, order_by: str | list[str] | None = None, limit_start: int | str | None = None, limit_page_length: int | str = 20, parent: str | None = None, debug: bool | int = False, as_dict: bool | int = True, or_filters: str | list[list] | dict[str, Any] | None = None, expand: str | list[str] | None = None)
```

**Logic:**
1. **Args passthrough** (built dict):
   ```python
   args = {
       "doctype": doctype,
       "parent_doctype": parent,  # NOTE: renamed from parent
       "fields": fields,
       "filters": filters,
       "or_filters": or_filters,
       "group_by": group_by,
       "order_by": order_by,
       "limit_start": limit_start,
       "limit_page_length": limit_page_length,
       "debug": debug,
       "as_list": not as_dict  # NOTE: inverted
   }
   ```

2. **Validation:** `validate_args(args)` — raises on invalid args

3. **Query:** `frappe.get_list(**args)`

4. **Expand (optional):**
   - If `expand` is provided and non-empty:
     - If `fields` exists and first field ≠ `"*"`: filter expand to fields that exist in fields list
     - Call `attach_expanded_links(doctype, _list, expand)` — mutates list in-place
     - Return mutated list

5. **Return:** `list[dict] | list[list]` depending on `as_dict`
   - `as_dict=True` (default) → `list[dict]`
   - `as_dict=False` → `list[list]`

---

### `get_count(doctype, filters, debug, cache) → int`

**Signature:**
```python
def get_count(doctype: str, filters: str | list | dict[str, Any] | None = None, debug: int | bool = False, cache: int | bool = False)
```

**Logic:**
1. **Setup form_dict** (for downstream function):
   ```python
   frappe.form_dict.doctype = doctype
   frappe.form_dict.filters = get_safe_filters(filters)
   frappe.form_dict.debug = debug
   ```

2. **Delegate:** call `get_count()` from `frappe.desk.reportview` (imports at top)

3. **Return:** integer count

---

### `set_value(doctype, name, fieldname, value) → dict`

**Signature:**
```python
@frappe.whitelist(methods=["POST", "PUT"])
def set_value(doctype: str, name: str | int, fieldname: str | dict[str, Any], value: Any | None = None)
```

**Logic:**

1. **Standard fields check:**
   ```python
   if fieldname in (frappe.model.default_fields + frappe.model.child_table_fields):
       raise frappe.ValidationError("Cannot edit standard fields")
   ```

2. **Value parsing** (fieldname can be string or dict):
   - If `value` is falsy (None, empty):
     - `values = fieldname` (use fieldname as dict)
     - If fieldname is string: try `json.loads(fieldname)` → if fails, use `{fieldname: ""}`
   - Else: `values = {fieldname: value}` (single key-value pair)

3. **Document retrieval + update:**
   - If **NOT child table** (`not frappe.get_meta(doctype).istable`):
     - `doc = frappe.get_doc(doctype, name)`
     - `doc.update(values)`
   - Else (child table):
     - Fetch parent: `frappe.db.get_value(doctype, name, ["parenttype", "parent"], as_dict=True)`
     - Load parent doc: `frappe.get_doc(parenttype, parent)`
     - Find child row: `child = doc.getone({"doctype": doctype, "name": name})`
     - Update child: `child.update(values)`

4. **Save and return:**
   ```python
   doc.save()
   return doc.as_dict()  # Parent doc dict if child was updated
   ```

**Return:** Full parent document dict (child updates save parent)

---

### `insert(doc) → dict`

**Signature:**
```python
@frappe.whitelist(methods=["POST", "PUT"])
def insert(doc: str | dict[str, Any] | None = None)
```

**Logic:**
1. If `doc` is string: `doc = json.loads(doc)`
2. Call `insert_doc(doc)` — helper function (see below)
3. Return: `insert_doc(doc).as_dict()`

**Return:** Inserted document dict

---

### `insert_many(docs) → list[str]`

**Signature:**
```python
@frappe.whitelist(methods=["POST", "PUT"])
def insert_many(docs: str | list[dict[str, Any]] | None = None)
```

**Logic:**
1. If `docs` is string: `docs = json.loads(docs)`
2. **Validate count:** if `len(docs) > 200`, raise error
3. Loop: for each `doc`, call `insert_doc(doc)` (helper), collect `.name` from result
4. Return: **list of document names (strings)**, not full dicts

```python
return [insert_doc(doc).name for doc in docs]
```

**Return:** `list[str]` — document names

---

### `submit(doc) → dict`

**Signature:**
```python
@frappe.whitelist(methods=["POST", "PUT"])
def submit(doc: str | dict[str, Any])
```

**Logic:**
1. If `doc` is string: `doc = json.loads(doc)`
2. `doc = frappe.get_doc(doc)`
3. `doc.submit()`
4. Return: `doc.as_dict()`

**Return:** Submitted document dict

---

### `cancel(doctype, name) → dict`

**Signature:**
```python
@frappe.whitelist(methods=["POST", "PUT"])
def cancel(doctype: str, name: str | int)
```

**Logic:**
1. `wrapper = frappe.get_doc(doctype, name)`
2. `wrapper.cancel()`
3. Return: `wrapper.as_dict()`

**Return:** Cancelled document dict

---

### `delete(doctype, name) → None`

**Signature:**
```python
@frappe.whitelist(methods=["DELETE", "POST"])
def delete(doctype: str, name: str | int)
```

**Logic:**
1. Call `delete_doc(doctype, name)` — helper function (see below)
2. **No return statement** (implicit None)

**Return:** `None`

**Helper `delete_doc(doctype, name)` — internal function:**
```python
def delete_doc(doctype, name):
    if frappe.is_table(doctype):
        # Child table deletion via parent
        values = frappe.db.get_value(doctype, name, ["parenttype", "parent", "parentfield"])
        if not values:
            raise frappe.DoesNotExistError
        parenttype, parent, parentfield = values
        parent = frappe.get_doc(parenttype, parent)
        if not parent.has_permission("write"):  # NOTE: checks write, not delete
            raise frappe.DoesNotExistError
        for row in parent.get(parentfield):
            if row.name == name:
                parent.remove(row)
                parent.save()
                break
    else:
        # Regular doctype deletion
        frappe.delete_doc(doctype, name, ignore_missing=False)
```

**Permissions:**
- Child table: requires **write** permission on parent (checks via `parent.has_permission("write")`)
- Regular: standard delete logic via `frappe.delete_doc` (which checks **delete** permission)

---

### `get_password(doctype, name, fieldname) → str`

**Signature:**
```python
@frappe.whitelist()
def get_password(doctype: str, name: str | int, fieldname: str)
```

**Logic:**
1. **Role check:** `frappe.only_for("System Manager")` — raises if not System Manager
2. Load doc: `frappe.get_lazy_doc(doctype, name)` — lazy-loads, permission NOT checked here
3. Return: `doc.get_password(fieldname)`

**Return:** Decrypted password string (or throws if field doesn't exist)

**Permission:** **System Manager only** (no doc-level permission check)

---

### `bulk_update(docs) → dict`

**Signature:**
```python
@frappe.whitelist(methods=["POST", "PUT"])
def bulk_update(docs: str)
```

**Logic:**
1. `docs = json.loads(docs)` — parse JSON string
2. Loop through each doc:
   - Pop `flags` key (if exists)
   - Load existing: `frappe.get_doc(doc["doctype"], doc["docname"])`
   - Update: `existing_doc.update(doc)`
   - Save: `existing_doc.save()`
   - On exception: append `{"doc": doc, "exc": traceback_string}` to `failed_docs` list (catch-all, no re-raise)
3. Return: `{"failed_docs": failed_docs}` — list of failed docs with exception traceback

**Return shape:**
```python
{
    "failed_docs": [
        {
            "doc": {...},  # original input doc
            "exc": "traceback string"
        },
        ...
    ]
}
```

**Important:** Partial success allowed — exceptions are caught and logged, not raised

---

### `validate_link_and_fetch(doctype, docname, fields_to_fetch, query, filters, **search_args) → dict`

**Signature:**
```python
@frappe.whitelist(methods=["GET", "POST"])
def validate_link_and_fetch(doctype: str, docname: str | int, fields_to_fetch: list[str] | str | None = None, query: str | None = None, filters: dict | list | str | None = None, **search_args)
```

**Logic:**

1. **Validation:** `if not docname: raise frappe.ValidationError("Document Name must not be empty")`

2. **Meta & field parsing:**
   - `meta = frappe.get_meta(doctype)`
   - `fields_to_fetch = frappe.parse_json(fields_to_fetch)` — may be None/list/parsed JSON

3. **Cache decision:** `can_cache = not fields_to_fetch and frappe.request.method == "GET"`

4. **Search validation:**
   - Update `search_args` with validation params:
     ```python
     search_args.update(
         as_dict=False,
         page_length=PAGE_LENGTH_FOR_LINK_VALIDATION,  # ~120
         txt=_(docname) if (query and meta.translated_doctype) else docname,
         for_link_validation=True
     )
     ```
   - Call `search_widget(doctype=doctype, query=query, filters=filters, **search_args)`
   - If search result empty: return `{}`

5. **Document existence check:**
   - If **virtual doctype** (`meta.get("is_virtual")`):
     - Try: `doc = frappe.get_doc(doctype, docname)` → `doc.check_permission("select")` → return `{"name": doc.name}`
     - Except DoesNotExistError: clear error, skip to next step
   - Else (normal doctype):
     - Fetch from DB: `frappe.db.get_value(doctype, docname, ["name", "parenttype" (if child)], as_dict=True)`
     - Result: `values` dict with correct case/type

6. **Permission validation via search result matching:**
   - Extract `name_to_compare = values["name"]` (correct case)
   - If search result length < PAGE_LENGTH_FOR_LINK_VALIDATION:
     - Check if `name_to_compare` exists in `search_result` list (search_result is `list[tuple]`, compare `item[0]`)
     - If NOT found: return `{}` (permission denied or filtered)

7. **Virtual doctype early return:** If `is_virtual_dt`, return `values` (name only, no field fetch)

8. **Cache header (if applicable):**
   - If `can_cache=True`: set response header `Cache-Control: private,max-age=1800,stale-while-revalidate=7200`

9. **Field fetch (optional):**
   - If `fields_to_fetch` is empty: return `values` (name only)
   - Else:
     - Call `get_value(doctype, fields_to_fetch, docname, parent=parent_doctype)` with caught exception handling
     - On PermissionError: clear error, show msgprint warning, return `values` (name only, no fields)
     - On success: `values.update(...)` with fetched fields, return `values`

**Return shape:**
```python
{
    "name": "DOC-001",
    "parenttype": "..."  # if child table
    # optional:
    "field1": "value1",
    "field2": "value2"
}
```

Empty result if validation fails: `{}`

---

## Summary Table

| Method | Permission Model | Null Handling | Return Type |
|--------|------------------|---------------|-------------|
| `get` | `check_permission()` | Stripped (no_nulls=True) | `dict` |
| `get_value` | `has_permission()` (early) | Depends on as_dict | scalar/dict/list/None |
| `get_single_value` | `has_permission()` (early) | None if field None | Any/None |
| `get_list` | Via frappe.get_list | Varies | list[dict]/list[list] |
| `get_count` | Via reportview.get_count | N/A | int |
| `set_value` | Via doc.save() | N/A | dict |
| `insert` | Via insert_doc → doc.insert() | Via as_dict | dict |
| `insert_many` | Via insert_doc → doc.insert() | N/A | list[str] (names) |
| `submit` | Via doc.submit() | Via as_dict | dict |
| `cancel` | Via doc.cancel() | Via as_dict | dict |
| `delete` | Child: write on parent; Normal: delete perm | N/A | None |
| `bulk_update` | Per-doc via doc.save() | N/A | {"failed_docs": list} |
| `get_password` | System Manager only (early) | N/A | str |
| `validate_link_and_fetch` | search_widget + optional get_value | Varies | dict |
