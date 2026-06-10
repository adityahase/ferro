# Frappe spec extract: naming

Now I have enough context. Let me create a comprehensive report with the exact specifications.

## Frappe Naming System - Precise Implementation Specification

Based on detailed code analysis of `/home/frappe/benches/bench-cpython314/apps/frappe/frappe/model/naming.py` and related test suites, here are the EXACT specifications for each Rust implementation:

---

### 1. `determine_consecutive_week_number(date)` — ISO Week Number Algorithm

**Location:** `naming.py` lines 410-420

**Exact Python Code:**
```python
def determine_consecutive_week_number(datetime):
	"""Determines the consecutive calendar week"""
	m = datetime.month
	# ISO 8601 calandar week
	w = datetime.strftime("%V")
	# Ensure consecutiveness for the first and last days of a year
	if m == 1 and int(w) >= 52:
		w = "00"
	elif m == 12 and int(w) <= 1:
		w = "53"
	return w
```

**Rust Implementation Spec:**
- **Input:** A `datetime` object with `.month` attribute and `.strftime()` method capability
- **Algorithm:**
  1. Extract `month` from datetime
  2. Get ISO week number via `datetime.strftime("%V")` → string (e.g., "01", "52")
  3. **Boundary Fixup:**
     - If `month == 1` AND `int(w) >= 52`: set `w = "00"` (Jan in ISO week 52/53 → becomes week 00, belongs to prev year in Frappe)
     - Else if `month == 12` AND `int(w) <= 1`: set `w = "53"` (Dec in ISO week 1/0 → becomes week 53, belongs to next year in Frappe)
  4. Return `w` as **2-character zero-padded string** (strftime `%V` already produces "01"-"53" format)

**Test Cases (from `test_naming.py:311-332`):**
- `2019-12-31` (Dec 31, ISO week 1) → `"53"`
- `2020-01-01` (Jan 1, ISO week 1) → `"01"` (normal Jan date)
- `2020-01-15` (Jan 15, ISO week 3) → `"03"`
- `2021-01-01` (Jan 1, ISO week 53) → `"00"` (Jan but ISO week 53)
- `2021-12-31` (Dec 31, ISO week 52) → `"52"` (normal Dec date)

---

### 2. `revert_series_if_last(key, name, doc=None)` — Series Counter Decrement Logic

**Location:** `naming.py` lines 441-486

**Exact Python Code:**
```python
def revert_series_if_last(key, name, doc=None):
	"""
	Reverts the series for particular naming series:
	* key is naming series		- SINV-.YYYY-.####
	* name is actual name		- SINV-2021-0001
	
	1. This function split the key into two parts prefix (SINV-YYYY) & hashes (####).
	2. Use prefix to get the current index of that naming series from Series table
	3. Then revert the current index.
	
	*For custom naming series:*
	1. hash can exist anywhere, if it exist in hashes then it take normal flow.
	2. If hash doesn't exit in hashes, we get the hash from prefix, then update name and prefix accordingly.
	
	*Example:*
	        1. key = SINV-.YYYY.-
	                * If key doesn't have hash it will add hash at the end
	                * prefix will be SINV-YYYY based on this will get current index from Series table.
	        2. key = SINV-.####.-2021
	                * now prefix = SINV-#### and hashes = 2021 (hash doesn't exist)
	                * will search hash in key then accordingly get prefix = SINV-
	        3. key = ####.-2021
	                * prefix = #### and hashes = 2021 (hash doesn't exist)
	                * will search hash in key then accordingly get prefix = ""
	"""
	if ".#" in key:
		prefix, hashes = key.rsplit(".", 1)
		if "#" not in hashes:
			# get the hash part from the key
			hash = re.search("#+", key)
			if not hash:
				return
			name = name.replace(hashes, "")
			prefix = prefix.replace(hash.group(), "")
	else:
		prefix = key

	if "." in prefix:
		prefix = parse_naming_series(prefix.split("."), doc=doc)

	count = cint(name.replace(prefix, ""))
	series = DocType("Series")
	current = (frappe.qb.from_(series).where(series.name == prefix).for_update().select("current")).run()

	if current and current[0][0] == count:
		frappe.db.sql("UPDATE `tabSeries` SET `current` = `current` - 1 WHERE `name`=%s", prefix)
```

**Rust Implementation Spec:**

**Preconditions:** Call ONLY when:
- Document insert **fails** (primary key violation, unique constraint, etc.)
- Document is being **deleted** (doctype has a naming_series or autoname with hash pattern)
- Naming pattern contains `#` hash characters (series counter pattern)

**Algorithm:**
1. **Parse naming series key:**
   - If key contains `".#"` (dot followed by hash):
     - Split on last `.` → `prefix` (everything before) and `hashes` (everything after)
     - If `hashes` contains **no** `#`:
       - Regex search `"#++"` in full key to find hash sequence
       - If found: remove that hash substring from `prefix`; remove `hashes` substring from `name`
   - Else (no `.#` in key):
     - `prefix = key` as-is

2. **Resolve prefix to counter key:**
   - If `prefix` contains `.` → parse it as naming_series parts (substitute date tokens like YY, MM, etc.)
   - Result is the **database Series table key** to look up

3. **Extract counter from name:**
   - `count = int(name.replace(prefix, ""))` → the numeric part after removing prefix

4. **Check and decrement in database:**
   - Query `tabSeries` table: `SELECT current FROM tabSeries WHERE name = ? FOR UPDATE`
   - **Only decrement if** `current == count` (i.e., this is the LAST allocated number)
   - If true: `UPDATE tabSeries SET current = current - 1 WHERE name = ?`
   - If false: **do nothing** (another thread/process already used this counter)

**Test Cases (from `test_naming.py:210-276`):**
- `key="TEST-.YYYY.-"`, `name="TEST-2024-00001"` → prefix resolves to `"TEST-2024-"`, count=1, revert if series.current==1
- `key="TEST-"`, `name="TEST-00003"` → prefix=`"TEST-"`, count=3, revert if current==3
- `key="TEST1-.#####.-2021-22"`, `name="TEST1-00003-2021-22"` → prefix=`"TEST1-"`, hashes=`"2021-22"` (no hash), count=3
- `key=".#####.-2021-22"`, `name="00003-2021-22"` → prefix=`""` (empty), count=3

---

### 3. `append_number_if_name_exists(doctype, name, fieldname, separator, filters)` — Duplicate Name Append Logic

**Location:** `naming.py` lines 530-554

**Exact Python Code:**
```python
def append_number_if_name_exists(doctype, value, fieldname="name", separator="-", filters=None):
	if not filters:
		filters = dict()
	filters.update({fieldname: value})
	exists = frappe.db.exists(doctype, filters)

	regex = f"^{re.escape(value)}{separator}\\d+$"

	if exists:
		last = frappe.db.sql(
			f"""SELECT `{fieldname}` FROM `tab{doctype}`
			WHERE `{fieldname}` {frappe.db.REGEX_CHARACTER} %s
			ORDER BY length({fieldname}) DESC,
			`{fieldname}` DESC LIMIT 1""",
			regex,
		)

		if last:
			count = str(cint(last[0][0].rsplit(separator, 1)[1]) + 1)
		else:
			count = "1"

		value = f"{value}{separator}{count}"

	return value
```

**Rust Implementation Spec:**

**Purpose:** When a document name already exists, append `-1`, `-2`, etc. to make it unique.

**Parameters:**
- `doctype`: DocType name (e.g., "Note")
- `value`: proposed name (e.g., "My Note")
- `fieldname`: field to check uniqueness on (default: "name")
- `separator`: delimiter before number (default: "-")
- `filters`: optional dict of additional WHERE conditions

**Algorithm:**
1. **Check existence:**
   - Build filter: `{fieldname: value}` + any additional filters
   - Query database: does a record with these filters exist?

2. **If does NOT exist:**
   - Return `value` unchanged

3. **If DOES exist:**
   - Build regex pattern: `^{escaped_value}{escaped_separator}\d+$`
     - Example: value=`"My Note"`, separator=`"-"` → pattern=`"^My Note\-\d+$"`
   - Query all existing names matching pattern:
     ```sql
     SELECT `{fieldname}` FROM `tab{doctype}`
     WHERE `{fieldname}` REGEX %s
     ORDER BY length({fieldname}) DESC, `{fieldname}` DESC
     LIMIT 1
     ```
   - **Order logic:**
     - Sort by string LENGTH descending (longer names first, e.g., "My Note-100" before "My Note-99")
     - Then sort by name descending (lexicographic)
     - Take the first (highest number)
   
4. **Calculate next number:**
   - If match found: extract number via `name.rsplit(separator, 1)[-1]` → increment by 1
   - If no match: start with `"1"`
   
5. **Return:**
   - `f"{value}{separator}{count}"` (e.g., `"My Note-1"`)

**Test Case (from `test_naming.py:36-50`):**
- `append_number_if_name_exists("Note", "Bottle")` where "Bottle" exists → `"Bottle-1"`
- `append_number_if_name_exists("Note", "Bottle", "title", "_")` → `"Bottle_1"` (custom fieldname & separator)

---

### 4. `_set_amended_name(doc)` / Amended Document Naming

**Location:** `naming.py` lines 557-574

**Exact Python Code:**
```python
def _set_amended_name(doc):
	amend_naming_rule = frappe.db.get_value(
		"Amended Document Naming Settings", {"document_type": doc.doctype}, "action", cache=True
	)
	if not amend_naming_rule:
		amend_naming_rule = frappe.get_single_value("Document Naming Settings", "default_amend_naming")

	if amend_naming_rule == "Default Naming":
		return

	am_id = 1
	am_prefix = doc.amended_from
	if frappe.db.get_value(doc.doctype, doc.amended_from, "amended_from"):
		am_id = cint(doc.amended_from.split("-")[-1]) + 1
		am_prefix = "-".join(doc.amended_from.split("-")[:-1])  # except the last hyphen

	doc.name = am_prefix + "-" + str(am_id)
	return doc.name
```

**Rust Implementation Spec:**

**Purpose:** When a document has `amended_from` field set (copy/amend flow), assign a sequential `-1`, `-2`, etc. suffix to the original name.

**Preconditions:**
- Document has `amended_from` field **set** (not null/empty)
- Applies ONLY if not using "Default Naming" amend rule

**Algorithm:**
1. **Determine amend rule:**
   - Query `Amended Document Naming Settings` table for `(document_type = doc.doctype)` → get `action` field
   - If not found OR empty: query singleton `Document Naming Settings` → get `default_amend_naming`
   - If rule is `"Default Naming"`: **abort** (return without setting name)

2. **Calculate amendment number:**
   - Default: `am_id = 1`, `am_prefix = doc.amended_from` (the original doc name)
   - **If the amended_from doc itself has an amended_from:**
     - Query DB: `SELECT amended_from FROM tab{doctype} WHERE name = doc.amended_from`
     - If exists:
       - Extract suffix: `last_suffix = int(doc.amended_from.split("-")[-1])`
       - `am_id = last_suffix + 1`
       - `am_prefix = "-".join(doc.amended_from.split("-")[:-1])` → remove last `-X` part
   
3. **Set name:**
   - `doc.name = f"{am_prefix}-{am_id}"`
   - Return the name

**Test Case (from `test_naming.py:278-309`):**
- Original: `doc.name = "ORIGINAL"`, submit, cancel
- Amendment: `amended_doc.amended_from = "ORIGINAL"` → `amended_doc.name = "ORIGINAL-1"`
- Second amendment: `amended_doc2.amended_from = "ORIGINAL-1"` → `amended_doc2.name = "ORIGINAL-2"`

---

### 5. Child Table Row Naming — Assignment During Document Save

**Location:** `document.py` lines 940-964 (method `set_new_name`), plus `naming.py` lines 141-198 (function `set_new_name`)

**Exact Python Code (document.py):**
```python
def set_new_name(self, force=False, set_name=None, set_child_names=True):
	"""Calls `frappe.naming.set_new_name` for parent and child docs."""

	if self.flags.name_set and not force:
		return

	autoname = self.meta.autoname or ""

	# If autoname has set as Prompt (name)
	if self.get("__newname") and autoname.lower() == "prompt":
		self.name = validate_name(self.doctype, self.get("__newname"))
		self.flags.name_set = True
		return

	if set_name:
		self.name = validate_name(self.doctype, set_name)
	else:
		set_new_name(self)

	if set_child_names:
		# set name for children
		for d in self.get_all_children():
			set_new_name(d)

	self.flags.name_set = True
```

**Exact Python Code (naming.py):**
```python
def set_new_name(doc):
	"""
	Sets the `name` property for the document based on various rules.

	1. If amended doc, set suffix.
	2. If `autoname` method is declared, then call it.
	3. If `autoname` property is set in the DocType (`meta`), then build it using the `autoname` property.
	4. If no rule defined, use hash.

	:param doc: Document to be named.
	"""

	doc.run_method("before_naming")

	meta = frappe.get_meta(doc.doctype)
	autoname = meta.autoname or ""

	if autoname.lower() not in ("prompt", "uuid") and not frappe.flags.in_import:
		doc.name = None

	if is_autoincremented(doc.doctype, meta):
		doc.name = frappe.db.get_next_sequence_val(doc.doctype)
		return

	if meta.autoname == "UUID":
		if not doc.name:
			doc.name = str(uuid7())
		elif isinstance(doc.name, UUID):
			doc.name = str(doc.name)
		elif isinstance(doc.name, str):  # validate
			try:
				UUID(doc.name)
			except ValueError:
				frappe.throw(_("Invalid value specified for UUID: {}").format(doc.name), InvalidUUIDValue)
		return

	if getattr(doc, "amended_from", None):
		_set_amended_name(doc)
		if doc.name:
			return

	elif getattr(doc.meta, "issingle", False):
		doc.name = doc.doctype

	if not doc.name:
		set_naming_from_document_naming_rule(doc)

	if not doc.name:
		doc.run_method("autoname")

	if not doc.name and autoname:
		set_name_from_naming_options(autoname, doc)

	# at this point, we fall back to name generation with the hash option
	if not doc.name:
		doc.name = make_autoname("hash", doc.doctype)

	doc.name = validate_name(doc.doctype, doc.name)
```

**Rust Implementation Spec for Child Table Row Naming:**

**Flow (from `document.py` line 690, during `db_insert()`):**
```
insert() → set_new_name(set_child_names=True)
  ├─ set_new_name(parent_doc)
  └─ for each child in parent_doc.get_all_children():
     └─ set_new_name(child_doc)
```

**Key Points for Child Documents:**

1. **Initialization (base_document.py:497-499):**
   - When a child is appended via `parent.append(fieldname, child_dict)`:
     - If no name exists: `child.__temporary_name = frappe.generate_hash(length=10)` (10-char base32hex random)
     - Set: `child.parent = parent.name`, `child.parenttype = parent.doctype`, `child.parentfield = fieldname`

2. **Naming Rule Determination (for child doctype):**
   - Get child's DocType `meta.autoname` setting
   - Apply same rules as parent (but to child's field values):
     - `"field:X"` → use child's field X value as name
     - `"naming_series:X"` → use child's naming_series field + counter
     - `"format:..."` → substitute child field values
     - `"hash"` → `make_autoname("hash", child.doctype)` → 10-char hash
     - No rule → hash fallback

3. **Name Persistence:**
   - Child's name is **NOT** auto-regenerated on save updates
   - Test case (`test_naming.py:62-91`):
     - Child appended with `some_fieldname="MyHash"` → name becomes `"MyHash"` (field:some_fieldname rule)
     - On parent save update, child's name **stays** `"MyHash"` (does not re-eval)
     - Only FIRST save assigns the name

4. **Function Call Chain for Child:**
   - `set_new_name(child)` → calls `naming.set_new_name(child_doc)` 
   - Which checks child's `meta.autoname` and applies the same logic as parent

**Summary:** Child rows are named during parent insert, calling the **same `set_new_name()` function** but with the child document object. The child's autoname rule (from its DocType definition) determines the naming strategy.

---

### Summary Table

| Function | Return Type | Key Logic |
|----------|-------------|-----------|
| `determine_consecutive_week_number(date)` | `str` (2-digit zero-padded) | ISO %V with Jan/Dec boundary fixups to "00"/"53" |
| `revert_series_if_last(key, name, doc)` | `None` (side effect: DB UPDATE) | Decrement Series counter only if it was the last allocated number |
| `append_number_if_name_exists(doctype, value, fieldname, separator, filters)` | `str` | Return `value` or `value{separator}{N}` if duplicate exists; find highest N via regex + ORDER BY length DESC |
| `_set_amended_name(doc)` | `str` (or None if "Default Naming") | Suffix original name with `-1`, `-2`, etc.; check if amended_from itself was amended |
| Child naming (via `set_new_name(child)`) | N/A (sets `doc.name`) | Resolve child's doctype autoname rule, apply to child's field values; use hash fallback |
