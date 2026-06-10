# Frappe spec extract: orm-filters

Perfect. Now let me create a comprehensive markdown report with all the filter parsing edge cases:

# Frappe Database Filter Parsing Spec for Rust Reimplementation

## Overview

Frappe supports multiple filter formats and operators. Filters are processed through a multi-layered system: `FilterTuple` (typed representation) → `Filters` (list) → `DatabaseQuery` (old system) or `Engine.query` (new pypika-based system) → SQL generation.

---

## 1. Filter Input Formats

### 1.1 **Dictionary Filters** (modern preferred)
```python
# Simple field = value (implies operator '=')
{"fieldname": value}
{"fieldname": "text_value"}
{"fieldname": 123}
{"fieldname": None}

# Explicit operator
{"fieldname": (operator, value)}
{"fieldname": ("in", [1, 2, 3])}
{"fieldname": ("between", [from, to])}
{"fieldname": ("like", "%pattern%")}
```

**Processing in `apply_dict_filters()` (query.py:528-534):**
- Iterates field/value pairs
- Default operator is `"="`
- If value is a `list|tuple`, unpacks as `(operator, value)`

### 1.2 **List Filters** (multiple formats)
```python
# Format A: [field, value] → implies operator '='
[field, value]

# Format B: [field, operator, value]
[field, operator, value]

# Format C: [doctype, field, operator, value]
[doctype, field, operator, value]

# Format D: [doctype, field, operator, value, _extra] (deprecated in v16)
# Extra element is silently ignored
```

**Processing in `apply_list_filters()` (query.py:515-526):**
- Matches on length (2, 3, 4, or 5 elements)
- Delegates to `_apply_filter()` with parsed args

### 1.3 **OR Filters**
```python
or_filters = [
    {"field1": value1},
    {"field2": value2}
]
```

**Processing in `apply_or_filters()` (query.py:488-513):**
1. Collects all filter criteria into a list
2. Combines with OR operator using `reduce()`: `cond1 | cond2 | cond3 ...`
3. Result: `WHERE (cond1) OR (cond2) OR (cond3)`

**Key difference from regular filters:** Regular filters are implicitly AND'd together; OR filters are all OR'd together.

### 1.4 **Nested Filters**
```python
# Format A: [cond1, op, cond2, op, cond3]
[
    [field1, op1, value1],  # first condition
    "and",                   # logical operator
    [field2, op2, value2],  # second condition
    "or",
    [field3, op3, value3]   # third condition
]

# Format B: Single group [[cond1, op, cond2]]
[[
    [field1, "=", value1],
    "or",
    [field2, "=", value2]
]]
```

**Processing in `apply_filters()` (query.py:379-483):**
1. Detects nested structure by checking if operators at odd indices are 'and'/'or'
2. Validates first element is a list (condition)
3. Calls `_parse_nested_filters()` which processes left-to-right with precedence (no nesting)
4. Combines criteria with `&` (AND) or `|` (OR) operators

### 1.5 **Special Case: List of Names**
```python
filters = ["name1", "name2", "name3"]
```

**Processing (query.py:412-415):**
- If all items are `FilterValue` (scalar types), converts to:
  ```python
  {"name": ("in", tuple(convert_to_value(f) for f in filters))}
  ```

---

## 2. Operators and Their Handling

### 2.1 **Comparison Operators**
| Operator | Example | Notes |
|----------|---------|-------|
| `=` | `["status", "=", "Draft"]` | Exact match; default |
| `!=` | `["status", "!=", "Submitted"]` | Not equal |
| `>` | `["qty", ">", 100]` | Greater than |
| `<` | `["qty", "<", 100]` | Less than |
| `>=` | `["qty", ">=", 100]` | Greater or equal |
| `<=` | `["qty", "<=", 100]` | Less or equal |
| `=<` | (alias for `<=`) | |
| `=>` | (alias for `>=`) | |

**Source:** `OPERATOR_MAP` in `operator_map.py:138-161`

### 2.2 **IN / NOT IN Operators**

#### Normal Case (list of values):
```python
["status", "in", ["Draft", "Submitted", "Amended"]]
```

**Processing in `func_in()` (operator_map.py:38-54):**
```python
def func_in(key, value):
    if isinstance(value, str):
        value = value.split(",")
    
    value = ["" if v is None else v for v in value]
    if "" in value:
        return key.isin(value) | key.isnull()  # NULL matches
    return key.isin(value)
```

**Key behavior:** If list contains `None`, it gets converted to `""`, and the condition becomes `field IN (..., '') OR field IS NULL`.

#### Edge Case: JSON String Value
```python
# These are equivalent:
["status", "in", '["Draft", "Submitted"]']    # JSON string
["status", "in", "Draft, Submitted"]          # Comma-separated string
```

**Processing in `FilterTuple.__new__()` (types/filter.py:113-118):**
```python
if operator in ("in", "not in") and isinstance(value, str):
    try:
        parsed = json.loads(value)
        value = parsed if isinstance(parsed, list) else value.split(",")
    except ValueError:
        value = value.split(",")  # Fall back to comma-split
```

**Order of parsing:**
1. Try `json.loads()` → if success and result is list, use list
2. If `json.loads()` fails or result is not a list, fall back to `.split(",")`
3. Each element is `.strip()`'d in `prepare_filter_condition()` (db_query.py:898)

#### Edge Case: None Value / Empty List
```python
["status", "in", None]        # Value is None
["status", "in", []]          # Empty list
```

**Processing in `_build_criterion_for_simple_filter()` (query.py:612-621):**
```python
if _operator.lower() in ("in", "not in"):
    if isinstance(_value, (list, tuple, set)) and len(_value) == 0:
        if _operator.lower() == "in":
            return RawCriterion("1=0")  # Always False → 0 rows
        else:  # not in
            return RawCriterion("1=1")  # Always True → all rows
```

**db_query.py compatibility mode (628-629):**
```python
if self.db_query_compat and _value is None and _operator.casefold() in ("in", "not in"):
    _value = ("",)  # Convert None to empty string tuple
```

#### NOT IN:
```python
["status", "not in", ["Draft", "Submitted"]]
```

**Processing in `func_not_in()` (operator_map.py:70-82):**
```python
def func_not_in(key, value):
    if isinstance(value, str):
        value = value.split(",")
    return key.notin(value)
```

**Difference from IN:** No NULL handling; NULL is excluded from the result set naturally.

---

### 2.3 **LIKE / NOT LIKE Operators**

#### Standard Usage:
```python
["name", "like", "%test%"]
["description", "not like", "spam%"]
```

**Processing in `prepare_filter_condition()` (db_query.py:972-981):**
```python
if f.operator.lower() in ("like", "not like") and isinstance(value, str):
    value = value.replace("\\", "\\\\").replace("%", "%%")
    # Escapes backslashes first, then % to prevent wildcard injection
```

**Escaping order:** `\` → `\\` THEN `%` → `%%`

This prevents:
- `value = "100%"` → becomes `"100%%"` (literal % in pattern)
- `value = "c:\path"` → becomes `"c:\\path"` (literal backslash)

**PostgreSQL:** Uses `ILIKE` (case-insensitive) instead of `LIKE` (query.py:659-664)

#### Wildcard Semantics:
- `%` = any sequence of characters (0+)
- `_` = exactly one character
- Backslash escapes the following character

**Edge case:** `like` vs `ilike` in PostgreSQL (query.py:659-664)
```python
if self.is_postgres and _operator.casefold() == "like":
    operator_fn = OPERATOR_MAP["ilike"]  # Use case-insensitive
else:
    operator_fn = OPERATOR_MAP[_operator.casefold()]
```

---

### 2.4 **BETWEEN Operator**

#### Standard Usage:
```python
["creation", "between", ["2024-01-01", "2024-12-31"]]
["age", "between", [18, 65]]
```

**Processing in `func_between()` (operator_map.py:98-108):**
```python
def func_between(key, value):
    return key[slice(*value)]  # Expands [a, b] to key[a:b] in pypika
```

**Expected value:** Exactly 2-element list/tuple `[from, to]`

#### DateTime / Date Conversion (query.py:597-607):
```python
if _operator.lower() == "between":
    if isinstance(_value, (list, tuple)) and len(_value) == 2:
        # For Datetime fields with date values, expand to datetime range
        _value = _apply_datetime_field_filter_conversion(_value, doctype, field)
        # e.g., [date(2024-01-01), date(2024-12-31)] →
        # [datetime(2024-01-01 00:00:00), datetime(2024-12-31 23:59:59.999999)]
    elif isinstance(_value, str):
        # Parse string form like "2024-01-01|2024-12-31"
        _value = tuple(v.strip().strip("'") for v in get_between_date_filter(...).split(" AND "))
```

**Key:** Date-to-Datetime conversion expands range:
- From: `date.min()` (00:00:00)
- To: `date.max()` (23:59:59.999999)

---

### 2.5 **IS Operator** (Set / Not Set)

#### Allowed Values:
- `"set"` — Field is not empty/NULL
- `"not set"` — Field is empty/NULL

```python
["assigned_to", "is", "set"]        # NOT NULL
["description", "is", "not set"]    # IS NULL or empty
```

**Processing in `func_is()` (operator_map.py:111-120):**
```python
def func_is(key, value):
    match cstr(value).lower():
        case "set":
            return key != ""  # field != ''
        case "not set":
            return key.isnull() | (key == "")  # IS NULL OR field = ''
        case _:
            raise ValueError("`is` operator only supports `set` and `not set` as value")
```

**db_query.py interpretation (950-958):**
```python
if f.operator.lower() == "is":
    fallback = "''"
    if f.value == "set":
        f.operator = "!="
        can_be_null = False
    elif f.value == "not set":
        f.operator = "="
        can_be_null = not getattr(df, "not_nullable", False)
    f.value = value = ""
```

**Result:**
- `is "set"` → `field != ''` (with NULL check suppressed)
- `is "not set"` → `field = ''` with NULL coalescing (unless field is `not_nullable`)

---

### 2.6 **Timespan / Previous / Next Operators**

#### Usage:
```python
["creation", "timespan", "1 week"]        # Last 1 week
["modified", "previous", "1 month"]       # Previous 1 month
["due_date", "next", "7 days"]            # Next 7 days
```

**Processing in `operator_map.py:123-134`:**
```python
def func_timespan(key, value):
    return func_between(key, get_timespan_date_range(value))

def get_date_range(operator, value):  # db_query.py:1450-1468
    timespan_map = {
        "1 week": "week",
        "1 month": "month",
        "3 months": "quarter",
        "6 months": "6 months",
        "1 year": "year",
    }
    period_map = {
        "previous": "last",
        "next": "next",
    }
    if operator != "timespan":
        timespan = f"{period_map[operator]} {timespan_map[value]}"
    else:
        timespan = value
    return get_timespan_date_range(timespan)
```

**Result:** Converted to `between` with computed date range.

---

### 2.7 **REGEX Operator**

```python
["email", "regex", "^[a-z]+@example\.com$"]
```

**Processing in `func_regex()` (operator_map.py:85-95):**
```python
def func_regex(key, value):
    return key.regex(value)  # Database-specific regex syntax
```

**Database support:** MySQL/MariaDB only (not standard SQL)

---

### 2.8 **Nested Set Hierarchy Operators**

For DocTypes with tree structure (`lft`, `rgt` fields):
- `"ancestors of"`
- `"not ancestors of"`
- `"descendants of"`
- `"not descendants of"`
- `"descendants of (inclusive)"`

**Processing in `_build_criterion_for_simple_filter()` (query.py:631-657):**
```python
if _operator in NESTED_SET_OPERATORS:
    # Fetch actual parent/child node IDs from hierarchy
    nodes = frappe.db.get_value(ref_doctype, docname, ["lft", "rgt"])
    
    if "descendants" in _operator:
        # Find all nodes between these lft/rgt
        nodes = frappe.get_all(ref_doctype,
            filters={"lft": [">", lft], "rgt": ["<", rgt]})
    else:  # ancestors
        nodes = frappe.get_all(ref_doctype,
            filters={"lft": ["<", lft], "rgt": [">", rgt]})
    
    # Convert to IN operator
    operator_fn = OPERATOR_MAP["not in" if "not" in _operator else "in"]
    return operator_fn(_field, nodes or ("",))
```

---

## 3. NULL Handling

### 3.1 **IFNULL Coalescing**

**When applied (query.py:705-725):**

```python
if self._should_apply_ifnull(target_doctype, filter_field_name, _operator, _value):
    fallback_sql = self._get_ifnull_fallback(target_doctype, filter_field_name)
    # Convert fallback SQL string to value
    if fallback_sql == "''":
        fallback_value = ""
    elif fallback_sql.startswith("'"):
        fallback_value = fallback_sql[1:-1]  # Strip quotes
    else:
        try:
            fallback_value = int(fallback_sql)
        except:
            fallback_value = fallback_sql
    
    _field = functions.IfNull(_field, ValueWrapper(fallback_value))
```

**NOT applied if:**
- `can_be_null = False` (for `name`, `modified`, `creation`, or field with `not_nullable`)
- Operator is `in`/`not in` with empty list
- Field already has `IfNull`/`Coalesce` function
- Value is `None` and operator is `!=` (uses `field.isnull()`)

### 3.2 **Special NULL Cases**

**When `value is None` and operator is `!=` (query.py:665-690):**
```python
if _value is None and isinstance(_field, Field):
    if operator_fn == builtin_operator.ne:
        # Don't use IFNULL; just check IS NOT NULL
        return _field.isnull()  # Actually checks IS NULL (note: returns IS NULL criterion)
    else:
        # For other operators with NULL value
        return _field.isnull()
```

Wait, re-reading: both branches return `.isnull()`. This looks like it should be `.isnotnull()` for the `!=` case, but the code as written returns `.isnull()` regardless. **This may be a bug or intentional.**

---

## 4. Field Type-Specific Processing

### 4.1 **Date Fields**

**Conversion (query.py:43-90):**
```python
def _apply_date_field_filter_conversion(value, operator, doctype, field):
    # For Date fieldtype, convert datetime values to date
    if operator.lower() == "between" and isinstance(value, (list, tuple)) and len(value) == 2:
        from_val, to_val = value
        if isinstance(from_val, datetime.datetime):
            from_val = from_val.date()
        if isinstance(to_val, datetime.datetime):
            to_val = to_val.date()
        return (from_val, to_val)
    elif isinstance(value, datetime.datetime):
        return value.date()
    return value
```

**Applied to:** Operators `between`, `>`, `<`, `>=`, `<=`, `=`, `!=` on Date fields

### 4.2 **Datetime Fields**

**Special expansion for `between` (query.py:93-128):**
```python
def _apply_datetime_field_filter_conversion(between_values, doctype, field):
    # If filtering Datetime field with date values, expand to full day range
    from_val = _convert_type_for_between_filters(from_val, set_time=datetime.time())  # 00:00:00
    to_val = _convert_type_for_between_filters(to_val, set_time=datetime.time(23, 59, 59, 999999))  # 23:59:59.999999
    return (from_val, to_val)
```

**Applied to:** `creation`, `modified`, or Datetime fieldtype with `between` operator

### 4.3 **Numeric Fields** (Int, Float, Currency, Check)

**Processing in `prepare_filter_condition()` (db_query.py:907-911):**
```python
if df and (df.fieldtype in ("Check", "Float", "Int", "Currency", "Percent")
        or getattr(df, "not_nullable", False)):
    can_be_null = False  # Skip IFNULL coalescing
```

**Value conversion (db_query.py:1000):**
```python
else:
    value = flt(f.value)  # Convert to float
```

### 4.4 **Text / Link / Data Fields**

**Processing (db_query.py:972-997):**
```python
elif f.operator.lower() in ("like", "not like") or (
    isinstance(f.value, str) and 
    (not df or df.fieldtype not in ["Float", "Int", "Currency", "Percent", "Check"])
):
    value = "" if f.value is None else f.value
    fallback = "''"
    # Apply LIKE escaping if needed
```

---

## 5. Filter Validation & Sanitization

### 5.1 **Operator Validation**

**Valid operators (utils/data.py:2295-2312):**
```
=, !=, >, <, >=, <=, like, not like, in, not in, is, between, timespan, previous, next
+ Nested Set operators (ancestors/descendants of)
+ Custom operators from hooks
```

### 5.2 **Field Name Validation**

**Simple field pattern (query.py:139):**
```python
SIMPLE_FIELD_PATTERN = re.compile(r"^\w+$")  # alphanumeric + underscore
```

**Backtick notation (query.py:167):**
```python
BACKTICK_FIELD_PARSE_REGEX = re.compile(r"^`tab([\w\s-]+)`\.(`?)(\w+)\2$")
# Matches: `tabDocType`.`fieldname` or `tabDocType Name`.fieldname
```

**Dot notation (for child/link fields):**
```python
"link_field.target_field"   # Link field → target doctype field
"child_table.field"         # Child table field
```

---

## 6. Edge Cases & Special Behaviors

### 6.1 **Empty String vs None in IN**

```python
["status", "in", ["Draft", "", "Submitted"]]
```

**Result:** `field IN ('Draft', '', 'Submitted') OR field IS NULL`

This is because `func_in()` converts `None` → `""`, then checks `if "" in value` to add NULL condition.

### 6.2 **Comma-Separated String in IN**

```python
["status", "in", "Draft,Submitted,Amended"]
```

**Processing:**
1. `FilterTuple.__new__()` detects string in `("in", "not in")` operator
2. Tries `json.loads("Draft,Submitted,Amended")` → fails (not valid JSON)
3. Falls back to `.split(",")` → `["Draft", "Submitted", "Amended"]`
4. Each element is later `.strip()`'d in `prepare_filter_condition()`

### 6.3 **Nested Filters Precedence**

```python
[
    ["field1", "=", "a"],
    "or",
    ["field2", "=", "b"],
    "and",
    ["field3", "=", "c"]
]
```

**Result:** `(field1 = 'a') OR (field2 = 'b') AND (field3 = 'c')`

**Important:** NO precedence grouping. Operators are applied left-to-right. So above becomes:
```
((field1 = 'a') OR (field2 = 'b')) AND (field3 = 'c')
```

Which is NOT equivalent to:
```
(field1 = 'a') OR ((field2 = 'b') AND (field3 = 'c'))
```

### 6.4 **Child Table Field Filtering**

```python
[
    {"child_table.fieldname": "value"},
    ["child_doctype", "child_field", "=", "value"]
]
```

**Processing (query.py:847-872):**
- If field contains `"."`, checks if it's a link field or child table field
- For child tables, adds a LEFT JOIN on `parenttype = doctype AND parent = name`
- Converts to qualified field: ``tabChild DocType`.`fieldname``

### 6.5 **Doctype-Qualified Filters**

```python
["DocType Name", "fieldname", "=", "value"]
["User", "email", "like", "%@example.com"]
```

**Processing:**
- 4-element list format allows specifying different doctype
- Used for filtering child table fields or linked records
- Field permission checks applied to specified doctype

---

## 7. Rust Implementation Checklist

### Critical Parsing Rules

1. **List detection:** Check length and element types:
   - Length 2: `[field, value]` → operator `=`
   - Length 3: `[field, op, value]` → check OPERATOR_MAP
   - Length 4: `[doctype, field, op, value]`
   - Length 5: deprecated, ignore 5th element

2. **OR filter detection:** Dict list where each dict becomes one condition combined with OR

3. **Nested filter detection:** List starting with a list, with 'and'/'or' at odd indices

4. **IN/NOT IN edge cases:**
   - Empty list `[]` → IN returns `1=0`, NOT IN returns `1=1`
   - None value → convert to `""` → if result has `""`, add OR NULL condition
   - String value: try `json.loads()`, fall back to `.split(",")`
   - Strip/trim each element

5. **LIKE escaping:**
   - Order: `\` → `\\` THEN `%` → `%%`
   - NOT `%%` → `%` then `\` → `\\` (wrong order)

6. **BETWEEN:** Exactly 2-element list/tuple required

7. **IS operator:** Only `"set"` or `"not set"` allowed

8. **Date/DateTime conversion:**
   - Date field + datetime value → truncate to date
   - Datetime field + date value + between → expand to `[date 00:00, date 23:59:59.999999]`

9. **NULL handling:**
   - Apply IFNULL unless: empty IN, operator `in`, field is `not_nullable`, operator `between`, operator `is`
   - Default fallback: `""`

10. **Field validation:**
    - Simple field: `\w+` only
    - Backtick: `` `tabDocType`.`field` ``
    - Dot notation: `link_field.target_field`

---

## Key Operator Code Locations

| Feature | File | Lines |
|---------|------|-------|
| `FilterTuple` parsing | `frappe/types/filter.py` | 62-144 |
| `Filters` class | `frappe/types/filter.py` | 152-276 |
| `Engine.apply_filters` | `frappe/database/query.py` | 379-486 |
| `Engine._build_criterion_for_simple_filter` | `frappe/database/query.py` | 552-725 |
| `func_in`, `func_not_in` | `frappe/database/operator_map.py` | 38-82 |
| `func_between`, `func_is` | `frappe/database/operator_map.py` | 98-120 |
| `prepare_filter_condition` (old system) | `frappe/model/db_query.py` | 793-1032 |
| `LIKE` escaping | `frappe/model/db_query.py` | 979-981 |
| `IN` parsing (JSON/CSV) | `frappe/types/filter.py` | 113-118 |
| Date/DateTime conversion | `frappe/database/query.py` | 43-128 |

---

## Summary Table: Operator Behavior

| Operator | NULL Handling | Empty List | Value Type | Notes |
|----------|---------------|-----------|-----------|-------|
| `=` | IFNULL | N/A | scalar | Match; default |
| `!=` | No IFNULL | N/A | scalar | Not match |
| `>`, `<`, `>=`, `<=` | No IFNULL for dates | N/A | scalar | Comparison |
| `in` | Replace None with "" + OR NULL | `1=0` | list/str | Match any; parse JSON first |
| `not in` | No special NULL | `1=1` | list/str | Exclude all |
| `like` | IFNULL | N/A | string | Escape: `\`→`\\` then `%`→`%%` |
| `not like` | IFNULL | N/A | string | Same escaping |
| `between` | No IFNULL | N/A | 2-tuple | Date/DateTime expansion |
| `is` | N/A | N/A | "set"/"not set" | Special: converts to `!=` or `=` |
| `regex` | IFNULL | N/A | string | MySQL/MariaDB only |

This spec covers all production-grade filter handling in Frappe. A Rust reimplementation should follow these rules precisely to maintain byte-for-byte compatibility with legacy `db_query` and feature-complete compatibility with modern `Engine.query` (pypika).
