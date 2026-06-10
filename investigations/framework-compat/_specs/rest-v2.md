# Frappe spec extract: rest-v2

Perfect! Now I have all the information I need. Let me compile the comprehensive spec.

Based on my analysis of the Frappe v2 REST API source code, here is the **precise spec for a Rust reimplementation**:

---

# Frappe v2 REST API Specification for Rust Reimplementation

## 1. Error Envelope Shape (v2 vs v1 Contrast)

### v2 Error Response Format
**Location**: `frappe/utils/response.py` lines 54–60 (in `report_error()`)

```json
{
  "errors": [
    {
      "type": "<ExceptionClassName>",          // e.g. "ValidationError", "PermissionError"
      "exception": "<multiline_traceback>",    // only if allow_traceback=true (dev/system user)
      "message": "<str>",                      // from message_log[i] if linked to exception
      "title": "<str>",                        // optional, from message_log
      ...other message_log keys...
    }
  ],
  "messages": [...],                           // v2-only: message_log array
  "debug": [{"message": "..."}, ...],         // if debug_log + traceback allowed
  "data": null,                                // absent on error
  "docs": [...]                                // optional: docs from failed bulk ops
}
```

**Key mechanism** (`_link_error_with_message_log`):
- Scan `frappe.message_log` for entry with matching `__frappe_exc_id` 
- Merge all message_log fields into the error object
- Remove `raise_exception` and `__frappe_exc_id` from error_log
- Clean up message_log by removing processed entries

### v1 Error Response Format (for contrast)
```json
{
  "exc_type": "<ExceptionClassName>",
  "_server_messages": "[[...], [...], ...]",  // JSON-encoded message_log items
  "exception": "<last_traceback_line>",       // only the final line
  "_exc_source": "...",                       // guess from error_log
  "exc": "[...]",                              // JSON-encoded error_log
  "_error_message": "...",
  "_debug_messages": "[...]"
}
```

### HTTP Status Code Mapping (Exception → Code)
From `frappe/exceptions.py`:
```
ValidationError              → 417 (includes subclasses)
FrappeTypeError              → 417
AuthenticationError           → 401
SessionExpired                → 401
PermissionError               → 403
DoesNotExistError             → 404
NameError (DuplicateEntry)    → 409
OutgoingEmailError            → 501
SessionStopped               → 503 (maintenance_mode)
InReadOnlyMode               → 503 (read-only flag set)
UnsupportedMediaType          → 415
CSRFTokenError                → 400
TooManyRequestsError          → 429
ServiceUnavailableError       → 503
RateLimitExceededError        → 429
(default/unmapped)            → 500
```

### Special Maintenance Mode Behavior
**Location**: `frappe/app.py` lines 153–156
```python
if frappe.local.conf.maintenance_mode:
    if frappe.local.conf.allow_reads_during_maintenance:
        setup_read_only_mode()  # Sets frappe.flags.read_only = True
    else:
        raise frappe.SessionStopped("Session Stopped")  # → 503
```

**When read-only is active**: Any write operations should throw `InReadOnlyMode` (→ HTTP 503).

---

## 2. Document CRUD Routes & Response Envelopes

### Route Table & Handlers
**Location**: `frappe/api/v2.py` lines 586–618

| Method | Route | Handler | Response |
|--------|-------|---------|----------|
| GET | `/api/v2/document/<doctype>` | `document_list()` | `{"data": [...], "has_next_page": bool}` |
| POST | `/api/v2/document/<doctype>` | `create_doc()` | `{"data": {...}}` |
| GET | `/api/v2/document/<doctype>/<name>/` | `read_doc()` | `{"data": {...}}` |
| PATCH/PUT | `/api/v2/document/<doctype>/<name>/` | `update_doc()` | `{"data": {...}}` |
| DELETE | `/api/v2/document/<doctype>/<name>/` | `delete_doc()` | `{"data": "ok"}` + HTTP 202 |
| GET | `/api/v2/document/<doctype>/<name>/copy` | `copy_doc()` | `{"data": {...}}` |
| GET/POST | `/api/v2/document/<doctype>/<name>/method/<method>/` | `execute_doc_method()` | `{"data": <return>, "docs": [...]}` |

### Response Envelope Structure
```json
{
  "data": <actual_data>,           // result of operation
  "docs": [...],                   // optional: modified docs appended to response
  "messages": [...],               // from message_log
  "debug": [...]                   // if debug logging enabled
}
```

### Document List (`document_list`)
**Location**: `frappe/api/v2.py` lines 93–184

**Query Parameters**:
```
fields: JSON string → list[str]    // which columns to fetch
filters: JSON string → dict|list   // WHERE clause (dict or advanced filter array)
order_by: str                      // SQL ORDER BY
start: int (default 0)             // OFFSET
limit: int (default 20)            // LIMIT (note: +1 fetched internally to detect has_next_page)
group_by: str                      // GROUP BY
as_dict: bool (default true)       // return as dict vs object
debug: bool (default false)        // include query in response
```

**Validation**:
- `fields` must be list or None → FrappeValueError (417) if dict/non-list
- `filters` must be dict/list or None → FrappeValueError (417)
- `order_by` must be str or None → FrappeValueError (417)
- `group_by` must be str or None → FrappeValueError (417)

**Response**:
```json
{
  "data": [
    {"name": "...", "field1": "...", ...},
    ...
  ],
  "has_next_page": true|false      // true if >limit records found
}
```

**Controller Customization**: 
- Doctype controllers can define `@staticmethod def get_list(query)` to customize the QueryBuilder
- If method returns non-None, validate it has a `.run()` method; else throw

### Create Document (`create_doc`)
**Location**: `frappe/api/v2.py` lines 195–204

**Request Body**: JSON object with doctype fields; `doctype` parameter removed before insert.

**Special Logic**:
```rust
if data.contains("name") && data["name"] is String|Int {
    doc.flags.name_set = True  // Preserve custom name
}
```

**Response**: `{"data": <doc.as_dict()>}`

### Copy Document (`copy_doc`)
**Location**: `frappe/api/v2.py` lines 207–215

**Query Parameters**:
```
ignore_no_copy: bool (default true)  // include no_copy fields in copy
```

**Response**:
```json
{
  "data": {...}  // copy with no_private_properties=True, no_nulls=True
}
```

### Update Document (`update_doc`)
**Location**: `frappe/api/v2.py` lines 218–231

**Request Body**: JSON object with fields to update.

**Special Logic**:
```rust
data.pop("flags")  // Always remove flags from input
doc.update(data)
doc.save()
doc.apply_fieldlevel_read_permissions()

// Child table support: if doc.parenttype exists, parent must be saved too
if doc.parenttype:
    frappe.get_doc(doc.parenttype, doc.parent).save()
```

**Response**: `{"data": <doc.as_dict()>}`

### Delete Document (`delete_doc`)
**Location**: `frappe/api/v2.py` lines 234–237

**Response**: 
```json
{
  "data": "ok",
  "http_status_code": 202
}
```

### Execute Document Method (`execute_doc_method`)
**Location**: `frappe/api/v2.py` lines 245–260

**Route Parameters**:
```
doctype: str
name: str
method: str (optional; fallback to frappe.form_dict["run_method"])
```

**Request Body** (POST): Additional kwargs for method.

**Validation**:
```rust
doc.is_whitelisted(method)  // method must be @frappe.whitelist or in controller
// Permission: GET → "read", POST → "write"
doc.check_permission(PERMISSION_MAP[request.method])
```

**Response**:
```json
{
  "data": <method_return>,
  "docs": [<doc.as_dict()>]  // Modified document appended
}
```

---

## 3. DocType Collection APIs

### Get Meta (`/api/v2/doctype/<doctype>/meta`)
**Location**: `frappe/api/v2.py` lines 240–242

**Route**: GET `/api/v2/doctype/<doctype>/meta`

**Validation**:
```rust
frappe.only_for("All")  // Restricted to "All" role (bypass check; all authenticated users pass)
```

**Response**:
```json
{
  "data": <frappe.get_meta(doctype)>  // DocType metadata object
}
```

**DocType Meta Structure** (standard Frappe):
```json
{
  "name": "DocTypeName",
  "fields": [
    {
      "fieldname": "...",
      "label": "...",
      "fieldtype": "String|Int|Link|...",
      "options": "...",
      "mandatory": true|false,
      "read_only": true|false,
      "no_copy": true|false,
      ...
    }
  ],
  "permissions": [
    {
      "role": "Administrator",
      "permlevel": 0,
      "read": 1,
      "write": 1,
      "submit": 1,
      ...
    }
  ],
  ...
}
```

### Get Count (`/api/v2/doctype/<doctype>/count`)
**Location**: `frappe/api/v2.py` lines 187–192

**Route**: GET `/api/v2/doctype/<doctype>/count`

**Query Parameters**: Same as `document_list` (filters, etc.)

**Response**:
```json
{
  "data": 42  // Integer count
}
```

---

## 4. Bulk Operations

### Bulk Delete by Doctype (`/api/v2/document/<doctype>/bulk_delete`)
**Location**: `frappe/api/v2.py` lines 263–321

**Route**: POST `/api/v2/document/<doctype>/bulk_delete`

**Request Body**:
```json
{
  "names": ["name1", 1, "name2", ...]  // list of str|int
}
```

**Validation**:
```rust
if !isinstance(names, list):
    raise FrappeValueError("'names' must be a list")  // 417

for name in names:
    if !isinstance(name, str|int):
        // Add to failed: {"name": name, "error": "'name' must be a string or integer"}
```

**Async Threshold**:
```python
if len(names) > get_bulk_operation_async_threshold(doctype):
    # Enqueue: frappe.enqueue("frappe.api.v2.execute_bulk_delete_docs", ...)
    # Return: {"job_id": "..."}
    # HTTP 202
```

**Synchronous Response** (names ≤ threshold):
```json
{
  "data": {
    "deleted": ["name1", "name2"],
    "failed": [
      {"name": "name3", "error": "PermissionError: ..."},
      ...
    ],
    "total": 3,
    "success_count": 2,
    "failure_count": 1
  }
}
```

**Per-item Rollback**: Each deletion uses a savepoint; on error, rollback that savepoint only.

### Bulk Delete Multi-DocType (`/api/v2/method/bulk_delete`)
**Location**: `frappe/api/v2.py` lines 324–391

**Route**: POST `/api/v2/method/bulk_delete`

**Request Body**:
```json
{
  "docs": [
    {"doctype": "DocType1", "name": "name1"},
    {"doctype": "DocType2", "name": "name2"},
    ...
  ]
}
```

**Validation**:
```rust
if !isinstance(docs, list):
    raise FrappeValueError("'docs' must be a list")  // 417

for item in docs:
    if !isinstance(item, dict):
        raise FrappeValueError("Each document must be a dictionary with 'doctype' and 'name' keys")  // 417
    
    doctype = item.get("doctype")
    name = item.get("name")
    
    if !isinstance(doctype, str):
        raise FrappeValueError("'doctype' must be a string")  // 417
    if !isinstance(name, str|int):
        raise FrappeValueError("'name' must be a string or integer")  // 417
    
    if isinstance(name, int):
        name = str(name)
```

**Async Threshold** (use global, no doctype specified):
```python
if len(docs) > get_bulk_operation_async_threshold():  # doctype=None
```

**Synchronous Response**:
```json
{
  "data": {
    "deleted": [
      {"doctype": "DocType1", "name": "name1"},
      ...
    ],
    "failed": [
      {"doctype": "DocType2", "name": "name2", "error": "..."},
      ...
    ],
    "total": 2,
    "success_count": 1,
    "failure_count": 1
  }
}
```

### Bulk Update by Doctype (`/api/v2/document/<doctype>/bulk_update`)
**Location**: `frappe/api/v2.py` lines 394–466

**Route**: POST `/api/v2/document/<doctype>/bulk_update`

**Request Body**:
```json
{
  "docs": [
    {
      "name": "name1",
      "field1": "newvalue1",
      "field2": 123,
      ...
    },
    ...
  ]
}
```

**Validation**:
```rust
if !isinstance(docs, list):
    raise FrappeValueError("'docs' must be a list")  // 417

for item in docs:
    if !isinstance(item, dict):
        raise FrappeValueError("Each update must be a dictionary with 'name' and field values")  // 417
    
    name = item.get("name")
    if !isinstance(name, str|int):
        raise FrappeValueError("'name' must be a string or integer")  // 417
```

**Per-item Logic**:
```rust
item_copy = item.copy()
item_copy.pop("name")
item_copy.pop("flags")  // Always remove

doc.update(item_copy)
doc.save()
doc.apply_fieldlevel_read_permissions()
frappe.response.docs.append(doc.as_dict())  // Collect for response
```

**Async Threshold**: `get_bulk_operation_async_threshold(doctype)` 

**Response**:
```json
{
  "data": {
    "updated": ["name1", "name2"],
    "failed": [
      {"name": "name3", "error": "Validation Error: ..."}
    ],
    "total": 3,
    "success_count": 2,
    "failure_count": 1
  },
  "docs": [
    <doc1.as_dict()>,
    <doc2.as_dict()>,
    ...
  ]
}
```

### Bulk Update Multi-DocType (`/api/v2/method/bulk_update`)
**Location**: `frappe/api/v2.py` lines 469–548

**Route**: POST `/api/v2/method/bulk_update`

**Request Body**:
```json
{
  "docs": [
    {
      "doctype": "DocType1",
      "name": "name1",
      "field1": "value1",
      ...
    },
    ...
  ]
}
```

**Validation** (identical to bulk delete multi-doctype, plus field validation):
```rust
if !isinstance(docs, list):
    raise FrappeValueError("'docs' must be a list")  // 417

for item in docs:
    if !isinstance(item, dict):
        raise FrappeValueError("Each document must be a dictionary with 'doctype', 'name', and field values")  // 417
    
    doctype = item.get("doctype")
    name = item.get("name")
    
    if !isinstance(doctype, str):
        raise FrappeValueError("'doctype' must be a string")  // 417
    if !isinstance(name, str|int):
        raise FrappeValueError("'name' must be a string or integer")  // 417
```

**Per-item Update**:
```rust
item_copy = item.copy()
item_copy.pop("doctype")
item_copy.pop("name")
item_copy.pop("flags")

doc.update(item_copy)
doc.save()
doc.apply_fieldlevel_read_permissions()
frappe.response.docs.append(doc.as_dict())

// Success recorded as:
updated.append({"doctype": doctype, "name": name})
```

**Response**:
```json
{
  "data": {
    "updated": [
      {"doctype": "DocType1", "name": "name1"},
      ...
    ],
    "failed": [
      {"doctype": "DocType2", "name": "name2", "error": "..."}
    ],
    "total": 2,
    "success_count": 1,
    "failure_count": 1
  },
  "docs": [<doc.as_dict()>, ...]
}
```

---

## 5. Read-Only / Maintenance Mode (503 Behavior)

**Location**: `frappe/app.py` lines 153–156 and `frappe/exceptions.py`

**Trigger Points**:

1. **Maintenance Mode with Reads Only**:
   ```python
   if frappe.local.conf.maintenance_mode:
       if frappe.local.conf.allow_reads_during_maintenance:
           frappe.flags.read_only = True
       else:
           raise SessionStopped("Session Stopped")  # → 503
   ```

2. **InReadOnlyMode Exception**:
   ```python
   class InReadOnlyMode(ValidationError):
       http_status_code = 503
   ```

**Expected Behavior**:

- When `frappe.flags.read_only = True`:
  - All read operations (GET) are allowed
  - All write operations (POST, PATCH, PUT, DELETE) should raise `InReadOnlyMode` or check before execution
  - Response HTTP 503 with error envelope (section 1)

**Response Example**:
```json
{
  "errors": [
    {
      "type": "InReadOnlyMode",
      "message": "Cannot write to database during read-only mode"
    }
  ]
}
```

---

## 6. Query Parameter Handling & Type Coercion

**Location**: `frappe/api/v2.py` lines 132–160

```rust
// For document_list:
fields = frappe.parse_json(args.get("fields", None))  // JSON string → list
filters = frappe.parse_json(args.get("filters", None))  // JSON string → dict|list
order_by = args.get("order_by", None)  // String, no parse
start = cint(args.get("start", 0))  // "0" → 0
limit = cint(args.get("limit", 20))  // "20" → 20
group_by = args.get("group_by", None)  // String, no parse
as_dict = bool(args.get("as_dict", True))  // "1"/"true" → True
debug = bool(args.get("debug", False))  // "1"/"true" → True
```

**Key Detail**: `limit + 1` is fetched internally to detect `has_next_page`, then truncated to `limit` in response.

---

## 7. Response Structure at Top Level

**Location**: `frappe/api/__init__.py` lines 58–69 and `frappe/utils/response.py` lines 91–106

```rust
// After endpoint execution:
if data is not None:
    frappe.response["data"] = data

// build_response("json") then:
as_json() {
    if frappe.local.response.http_status_code:
        response.status_code = frappe.local.response["http_status_code"]
        del frappe.local.response["http_status_code"]
    
    response.mimetype = "application/json"
    response.data = orjson_dumps(frappe.local.response, default=json_handler)
}
```

**Final JSON Wrapper**:
```json
{
  "data": <endpoint_return>,
  "docs": [...],                    // optional, initialized as []
  "errors": [...],                  // only on error (v2)
  "messages": [...],                // optional
  "debug": [...],                   // optional
  "_server_messages": "...",        // v1 only
  "_exc_source": "...",             // v1 only
  "_error_message": "...",          // v1 only
  "http_status_code": 200           // present, then deleted before JSON
}
```

---

## Summary of Critical Implementation Points

1. **Error Handling**: Use `errors` array (v2 only); map Python exception classes → HTTP status codes; link message_log entries by `__frappe_exc_id`.

2. **Response Envelope**: All v2 responses wrap actual data in `{"data": ...}`; optional `docs` array for bulk/method results.

3. **Validation**: FrappeValueError (417) for malformed list/dict parameters; InvalidationError for field-level issues.

4. **Bulk Async**: Check threshold; if exceeded, enqueue job and return `{"job_id": "..."}` + HTTP 202.

5. **Read-Only**: Check `frappe.flags.read_only`; block writes with 503.

6. **Savepoints**: Each bulk item uses per-item savepoint for granular rollback.

7. **Controller Integration**: Allow doctype controllers to override `get_list(query)` method.

8. **Field-Level Permissions**: Call `doc.apply_fieldlevel_read_permissions()` before returning doc.

---
