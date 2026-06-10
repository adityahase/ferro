# Frappe spec extract: document-lifecycle

Perfect! Now I have all the critical validation code. Let me compile the markdown spec:

---

# Frappe Document Validation Spec for Rust Reimplementation

## 1. Link Validation (`validate_links` / `_validate_links`)

**Location**: `frappe/model/document.py:1538-1559`

### Validation Logic
```python
def _validate_links(self):
    if self.flags.ignore_links or self._action == "cancel":
        return

    # Pre-fetch all link values in bulk
    self._prefetch_link_values()
    link_cache = getattr(self, "_link_value_cache", None)

    invalid_links, cancelled_links = self.get_invalid_links(link_value_cache=link_cache)

    for d in self.get_all_children():
        result = d.get_invalid_links(is_submittable=self.meta.is_submittable, link_value_cache=link_cache)
        invalid_links.extend(result[0])
        cancelled_links.extend(result[1])

    if invalid_links:
        msg = ", ".join(each[2] for each in invalid_links)
        frappe.throw(_("Could not find {0}").format(msg), frappe.LinkValidationError)

    if cancelled_links:
        msg = ", ".join(each[2] for each in cancelled_links)
        frappe.throw(_("Cannot link cancelled document: {0}").format(msg), frappe.CancelledLinkError)
```

### Link Field Validation (`get_invalid_links` in base_document.py:1015-1132)

**Fields Validated**: 
- All `Link` type fields (from `meta.get_link_fields()`)
- All `Dynamic Link` type fields (from `meta.get("fields", {"fieldtype": "Dynamic Link"})`)

**For each link field**:

1. **Get target doctype**:
   - For Link fields: use `df.options` (the doctype name)
   - For Dynamic Link fields: use value of field specified in `df.options` (dynamic reference)

2. **Check if document exists**:
   - Execute: `SELECT * FROM tab{doctype} WHERE name = {docname}` (via `_fetch_link_values`)
   - If target is a Single doctype: always exists, set `values.name = doctype`
   - If not found: append to `invalid_links` with tuple `(df.fieldname, docname, message)`

3. **Check if document is cancelled** (only if parent is submittable AND target is submittable):
   ```python
   check_docstatus = is_submittable and frappe.get_meta(doctype).is_submittable
   ```
   - If check enabled AND target's `docstatus` is 2 (cancelled):
   - Append to `cancelled_links` (except for "amended_from" field)
   - Error message: `"Cannot link cancelled document: {0}"`

4. **Exception Types**:
   - `frappe.LinkValidationError` if link target doesn't exist
   - `frappe.CancelledLinkError` if link target is cancelled

5. **HTTP Status**: Returns 417 (Expectation Failed) via frappe.throw()

---

## 2. Set-Only-Once Field Enforcement (`validate_set_only_once`)

**Location**: `frappe/model/document.py:1110-1136`

### Validation Logic
```python
def validate_set_only_once(self):
    """Validate that fields are not changed if not in insert"""
    set_only_once_fields = self.meta.get_set_only_once_fields()

    if set_only_once_fields and self._doc_before_save:
        # document exists before saving
        for field in set_only_once_fields:
            fail = False
            value = self.get(field.fieldname)
            original_value = self._doc_before_save.get(field.fieldname)

            if field.fieldtype in table_fields:
                fail = not self.is_child_table_same(field.fieldname)
            elif field.fieldtype in ("Date", "Datetime", "Time"):
                fail = str(value) != str(original_value)
            else:
                fail = value != original_value

            if fail:
                frappe.throw(
                    _("Value cannot be changed for {0}").format(
                        frappe.bold(_(self.meta.get_label(field.fieldname)))
                    ),
                    exc=frappe.CannotChangeConstantError,
                )
```

### Rules
- **Only runs on update** (when `_doc_before_save` exists; skipped on insert)
- **Identifies set-only-once fields**: from `meta.get_set_only_once_fields()`
- **Comparison logic**:
  - **Table fields**: compare entire child table (via `is_child_table_same()`) — exact array equality
  - **Date/Datetime/Time fields**: convert both to string, compare as strings
  - **Other fields**: direct equality comparison

- **On change detected**:
  - Error message: `"Value cannot be changed for {0}"` (where {0} is field LABEL via `meta.get_label(fieldname)`)
  - Exception type: `frappe.CannotChangeConstantError`

---

## 3. Optimistic Concurrency Check (`check_if_latest`)

**Location**: `frappe/model/document.py:1283-1312`

### Validation Logic
```python
def check_if_latest(self):
    """Checks if `modified` timestamp provided by document being updated is same as the
    `modified` timestamp in the database. If there is a different, the document has been
    updated in the database after the current copy was read. Will throw an error if
    timestamps don't match."""

    self.load_doc_before_save(raise_exception=True)

    if not hasattr(self, "_action"):
        self._action = "save"

    previous = self._doc_before_save
    # previous is None for new document insert
    if not previous and self._action != "discard":
        self.check_docstatus_transition(0)
        return

    if cstr(previous.modified) != cstr(self._original_modified):
        frappe.msgprint(
            _(f"Error: {self.name} ({self.doctype}) has been modified after you have opened it")
            + (f" ({previous.modified}, {self.modified}). ")
            + _("Please refresh to get the latest document."),
            raise_exception=frappe.TimestampMismatchError,
        )

    if not self.meta.issingle and self._action != "discard":
        self.check_docstatus_transition(previous.docstatus)
```

### Comparison Details
- **Payload modified**: stored in `self._original_modified` (set during `set_user_and_timestamp()` before validation)
- **DB modified**: fetched from `self._doc_before_save.modified` (loaded by `load_doc_before_save()`)
- **Comparison**: `cstr(previous.modified) != cstr(self._original_modified)` (string comparison)
- **On mismatch**:
  - Error message: `"Error: {name} ({doctype}) has been modified after you have opened it ({prev_modified}, {curr_modified}). Please refresh to get the latest document."`
  - Exception type: `frappe.TimestampMismatchError`
  - HTTP Status: 409 (Conflict)

### Skipped When
- Action is "discard"
- Document is new (`_doc_before_save` is None)

---

## 4. Mandatory/Required Field Validation (`_validate_mandatory`)

**Location**: `frappe/model/document.py:1389-1410` and base_document.py:969-1013

### Validation Logic
```python
def _validate_mandatory(self):
    if self.flags.ignore_mandatory:
        return

    missing = self._get_missing_mandatory_fields()
    for d in self.get_all_children():
        missing.extend(d._get_missing_mandatory_fields())

    if not missing:
        return

    for idx, msg in missing:
        msgprint(msg)

    if frappe.flags.print_messages:
        print(self.as_json().encode("utf-8"))

    raise frappe.MandatoryError(
        "[{doctype}, {name}]: {fields}".format(
            fields=", ".join(each[0] for each in missing), doctype=self.doctype, name=self.name
        )
    )
```

### Field Detection (`_get_missing_mandatory_fields` in base_document.py:969-1013)
```python
def _get_missing_mandatory_fields(self):
    def get_msg(df):
        if df.fieldtype in table_fields:
            return _("Error: Data missing in table {0}").format(_(df.label, context=df.parent))

        elif self.get("parentfield"):  # child table row
            return _("Error: {0} Row #{1}: Value missing for: {2}").format(
                frappe.bold(_(self.doctype)),
                self.idx,
                _(df.label, context=df.parent),
            )

        return _("Error: Value missing for {0}: {1}").format(_(df.parent), _(df.label, context=df.parent))

    def has_content(df):
        value = cstr(self.get(df.fieldname))
        has_text_content = strip_html(value).strip()
        has_img_tag = "<img" in value
        has_text_or_img_tag = has_text_content or has_img_tag

        if df.fieldtype == "Text Editor" and has_text_or_img_tag:
            return True
        elif df.fieldtype == "Code" and df.options == "HTML" and has_text_or_img_tag:
            return True
        elif df.fieldtype == "Check":
            return True  # Checkboxes can't be mandatory
        else:
            return has_text_content

    missing = []

    for df in self.meta.get("fields", {"reqd": ("=", 1)}):
        if self.get(df.fieldname) in (None, []) or not has_content(df):
            missing.append((df.fieldname, get_msg(df)))

    # check for missing parent and parenttype
    if self.meta.istable:
        for fieldname in ("parent", "parenttype"):
            if not self.get(fieldname):
                missing.append((fieldname, get_msg(_dict(label=fieldname))))

    return missing
```

### Rules
- **Identifies mandatory fields**: from `meta.get("fields", {"reqd": 1})`
- **Empty check**: field value in `(None, [])` OR `not has_content(df)`
- **Content validation** (special for rich-text fields):
  - Text Editor: has content if HTML-stripped text OR `<img>` tag exists
  - Code (HTML): has content if HTML-stripped text OR `<img>` tag exists
  - Check: always has content (checkboxes default to 0)
  - Others: has content if HTML-stripped text is non-empty

- **For child tables**: also validates `parent` and `parenttype` fields are set

- **Error Message Format**:
  - **Parent table field**: `"Error: Data missing in table {LABEL}"`
  - **Child table row**: `"Error: {DOCTYPE} Row #{IDX}: Value missing for: {LABEL}"`
  - **Parent doc field**: `"Error: Value missing for {DOCTYPE}: {LABEL}"`
  - Uses `_(df.label, context=df.parent)` to get translated field LABEL (not fieldname)

- **Exception Type**: `frappe.MandatoryError`
- **Exception Message**: `"[{doctype}, {name}]: {field1}, {field2}, ..."`

---

## 5. DocStatus Transitions and Immutable Fields After Submit

**Location**: `frappe/model/document.py:1314-1361`

### Valid DocStatus Transitions
```python
def check_docstatus_transition(self, from_docstatus):
    """Ensures valid `docstatus` transition.
    Valid transitions are (number in brackets is `docstatus`):

    - Save (0) > Save (0)
    - Save (0) > Submit (1)
    - Submit (1) > Submit (1)
    - Submit (1) > Cancel (2)
    """
```

### Transition Rules
| From → To | Allowed | Action | Notes |
|-----------|---------|--------|-------|
| 0 → 0 | ✓ | save | Draft to Draft |
| 0 → 1 | ✓ | submit | Draft to Submitted (requires is_submittable) |
| 0 → 2 | ✗ | — | Cannot skip submit, direct cancel forbidden |
| 1 → 1 | ✓ | update_after_submit | Submitted stays Submitted |
| 1 → 2 | ✓ | cancel | Submitted to Cancelled |
| 1 → 0 | ✗ | — | Cannot revert to Draft |
| 2 → * | ✗ | — | Cannot edit cancelled document |

### Immutable Fields After Submit
- **Fields marked `allow_on_submit = False`** cannot be changed after submit
- Validated in `_validate_update_after_submit()` (base_document.py:1326-1361)
- Exception: `frappe.UpdateAfterSubmitError`
- Error message: `"{Row #X:} Not allowed to change {LABEL} after submission from {OLD_VALUE} to {NEW_VALUE}"`

### Rules for Submit Validation
- DocType must have `is_submittable = True` to transition 0→1
- Requires `check_permission("submit")` before transition
- For update after submit (1→1): also requires `check_permission("submit")`
- For cancel (1→2): requires `check_permission("cancel")`

---

## Summary of Exception Types

| Exception Type | HTTP Status | Trigger |
|---|---|---|
| `LinkValidationError` | 417 | Link target document not found |
| `CancelledLinkError` | 417 | Link target is cancelled (submittable only) |
| `CannotChangeConstantError` | 417 | Set-only-once field changed on update |
| `TimestampMismatchError` | 409 | Optimistic concurrency conflict |
| `MandatoryError` | 417 | Required field missing |
| `UpdateAfterSubmitError` | 417 | Non-allow-on-submit field changed after submit |
| `DocstatusTransitionError` | 417 | Invalid docstatus state transition |

---

## Call Sequence During Insert/Update

### INSERT (new document):
1. `check_permission("create")`
2. `check_if_latest()` — validates docstatus transition 0→0 only
3. `_validate_links()` — checks all Link/DynamicLink fields exist (non-cancelled)
4. `before_insert()` hook
5. `run_before_save_methods()`:
   - `validate()` user hook
   - `before_save()` hook
   - `_validate()` framework validations:
     - `_validate_mandatory()` — checks required fields
     - `_validate_data_fields()` — phone/email/URL validation
     - `_validate_selects()` — select option validation
     - `_validate_non_negative()` — numeric bounds
     - `_validate_length()` — string/int length limits
     - `_validate_code_fields()` — Python syntax validation
     - `_sync_autoname_field()` — keep naming field in sync
     - `_extract_images_from_text_editor()` — extract embedded images
     - `_sanitize_content()` — XSS prevention
     - `_save_passwords()` — hash password fields
     - `validate_workflow()` — workflow state validation
6. `db_insert()` — save to database

### UPDATE (existing document):
1. `check_permission("write", "save")`
2. `check_if_latest()` — **optimistic concurrency check** + docstatus transition validation
3. `_validate_links()` — checks all Link/DynamicLink fields
4. `before_save()` hook
5. `validate()` user hook
6. `run_before_save_methods()` — same as INSERT
7. If docstatus 1→2 (cancel): `validate_update_after_submit()` skipped
8. If docstatus 1→1 (update_after_submit): `validate_update_after_submit()` — checks `allow_on_submit` fields
9. `validate_set_only_once()` — **checks no set-only-once fields changed**
10. `db_update()` — save to database
