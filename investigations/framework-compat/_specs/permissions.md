# Frappe spec extract: permissions

Perfect. Now let me create a comprehensive specification document based on the code I've read.

---

# Frappe Permissions System — Rust Reimplementation Spec

This is a precise specification of Frappe's permission system for exact Rust implementation. Transcribed from `/home/frappe/benches/bench-cpython314/apps/frappe/frappe/permissions.py` and related files.

---

## 1. `get_roles(user)` — Complete Specification

**Function Signature (Python):**
```python
def get_roles(user=None, with_standard=True):
```

**Logic Flow:**

```
Input: user (str or None)
Output: List[str] — roles assigned to user

1. If user is None:
   user = frappe.session.user

2. If user == "Guest" OR user is falsy (""):
   RETURN [GUEST_ROLE]

3. Otherwise:
   roles = []

   IF user == "Administrator":
      roles = all_roles_from_database()  # SELECT DISTINCT name FROM tabRole
   ELSE:
      # Query Has Role table for explicit user-assigned roles
      SELECT role FROM tabHasRole
      WHERE parenttype='User' AND parent=user AND role NOT IN AUTOMATIC_ROLES
      
      roles = result list
      
      # Always add these automatic roles to non-Admin users:
      roles.append("All")          # ALL_USER_ROLE constant
      roles.append("Guest")        # GUEST_ROLE constant
      
      # System User → Desk User mapping
      IF is_system_user(user):
         roles.append("Desk User")  # SYSTEM_USER_ROLE constant

4. If with_standard=False:
   filter out AUTOMATIC_ROLES = ("Guest", "All", "Desk User", "Administrator")

5. Cache result at key ("roles", user) with TTL

RETURN roles
```

**Constants (lines 33-40):**
```python
GUEST_ROLE = "Guest"
ALL_USER_ROLE = "All"
SYSTEM_USER_ROLE = "Desk User"
ADMIN_ROLE = "Administrator"

AUTOMATIC_ROLES = (GUEST_ROLE, ALL_USER_ROLE, SYSTEM_USER_ROLE, ADMIN_ROLE)
```

**Special Cases:**

- **Administrator**: Gets ALL roles in the system (database query to all Roles, not capped by HasRole table). Always returns `with_standard=True` effectively.
- **Guest**: Returns only `[GUEST_ROLE]` (bypasses everything else).
- **System User Detection** (line 896):
  ```python
  def is_system_user(user: str | None = None) -> bool:
      return frappe.get_cached_value("User", user or frappe.session.user, "user_type") == "System User"
  ```
  Checks the `user_type` field in `tabUser`.

---

## 2. `has_permission(doctype, ptype, doc, user)` — High-Level Flow

**Function Signature:**
```python
def has_permission(
    doctype,
    ptype="read",
    doc=None,
    user=None,
    *,
    parent_doctype=None,
    print_logs=True,
    debug=False,
    ignore_share_permissions=False,
) -> bool
```

**Decision Tree (lines 79–224):**

```
1. INITIAL SHORTCUTS:
   IF user is None:
      user = frappe.session.user

   IF user == "Administrator":
      RETURN True  ✓ (all permissions granted)

   IF ptype == "share" AND system_settings["disable_document_sharing"] == 1:
      RETURN False  ✗ (sharing disabled globally)

2. HANDLE CHILD DOCTYPES (frappe.is_table(doctype)):
   Call has_child_permission(...)
   RETURN result
   [Checks parent_doctype permissions instead]

3. GET DOCTYPE METADATA:
   meta = frappe.get_meta(doctype)

4. IF SINGLE DOCTYPE (meta.issingle) AND no doc passed:
   doc = doctype  (for singles, docname == doctype)

5. PERMISSION CHECK — TWO PATHS:

   PATH A: IF doc is provided (checking specific document):
   ────────────────────────────────────────────────────
      IF doc is str/int:
         doc = frappe.get_lazy_doc(doctype, doc)
      
      permissions = get_doc_permissions(doc, user=user, ptype=ptype)
      perm = permissions.get(ptype)
      
      IF NOT perm:
         RETURN False  ✗ (no doc-level permission)
      
      [Continue to DocShare fallback below]

   PATH B: IF no doc provided (checking doctype-level):
   ──────────────────────────────────────────────────────
      IF ptype in ("submit", "import"):
         meta_checks = validate_doctype_property(ptype)
         IF NOT meta_checks:
            RETURN False  ✗
      
      permissions = get_role_permissions(meta, user=user)
      perm = permissions.get(ptype)
      
      IF NOT perm:
         [Continue to DocShare fallback below]

6. DOCSHARE FALLBACK (if perm still not granted):
   IF NOT perm AND NOT ignore_share_permissions:
      
      share_rights = ["read", "write", "share", "submit", "email", "print"]
      custom_rights = get_doctype_ptype_map().get(doctype, [])
      
      IF ptype NOT IN (share_rights + custom_rights):
         RETURN False  ✗ (this ptype cannot be shared)
      
      IF doc is provided:
         # Check if THIS SPECIFIC DOC is shared with user
         shared_docs = frappe.share.get_shared(
            doctype,
            user,
            rights=[ptype],  # or "read" if ptype in ("email", "print")
            filters=[["share_name", "=", doc.name]]
         )
         IF shared_docs exists:
            perm = True  ✓ (document shared explicitly)
      ELSE:
         # Check if ANY doc of this doctype is shared
         IF frappe.share.get_shared(doctype, user, rights=[ptype], limit=1) exists:
            perm = True  ✓ (at least one doc shared, allow doctype access)

7. SELECT FALLBACK:
   IF NOT perm AND ptype == "select":
      perm = has_permission(doctype, ptype="read", doc=doc, user=user, ...)
      [select is implied by read]

8. RETURN bool(perm)
```

**Permission Grant Order (in priority):**
1. Administrator bypass (always True)
2. Document owner + if_owner rule
3. Role-based permissions (from DocPerm/Custom DocPerm)
4. User Permissions (restrict by linked fields)
5. DocShare (explicit per-document sharing)
6. select → read fallback

**Over-grant vs Under-grant:**
- **Over-grant**: Role permissions may grant more than actual access due to bypass (rare in normal setup).
- **Under-grant**: User Permissions and if_owner rules RESTRICT access (can deny higher permissions). DocShare grants only specific perms.

---

## 3. `get_doc_permissions(doc, user, ptype)` — Document-Level Permission Evaluation

**Function Signature (line 227):**
```python
def get_doc_permissions(doc, user=None, ptype=None, debug=False):
    """Return a dict of evaluated permissions for given doc like {"read":1, "write":1}"""
```

**Flow (lines 227–279):**

```
Input: doc (Document object), user (str), ptype (str optional)
Output: dict — {ptype: 0|1, "if_owner": {ptype: 0|1}, ...}

1. IF user is None:
   user = frappe.session.user

2. CONTROLLER PERMISSION CHECK:
   permissions = has_controller_permissions(doc, ptype, user)
   IF NOT permissions:
      RETURN {ptype: 0}  ✗ (controller veto)

3. GET ROLE PERMISSIONS:
   is_owner = (doc.owner.lower() == user.lower())
   base_permissions = get_role_permissions(doc.doctype, user, is_owner=is_owner)
   permissions = deepcopy(base_permissions)

4. APPLY DOCTYPE CONSTRAINTS:
   IF NOT meta.is_submittable:
      permissions["submit"] = 0
   
   IF NOT meta.allow_import:
      permissions["import"] = 0

5. APPLY OWNER OVERRIDE (if_owner rules):
   IF permissions.get("has_if_owner_enabled"):
      # if_owner rules exist in role config
      IF is_owner:
         permissions.update(permissions["if_owner"])
         [user gets additional owner-only permissions]

6. USER PERMISSION RESTRICTIONS:
   IF NOT has_user_permission(doc, user):
      IF is_owner:
         # Owner still gets if_owner perms, but nothing else
         permissions = permissions.get("if_owner", {})
         permissions["create"] = 0  [owner cannot create via if_owner]
      ELSE:
         # Non-owner gets nothing
         permissions = {}

7. RETURN permissions  [dict of {ptype: 0|1} for all rights]
```

**Key Detail — if_owner Override Logic (lines 254–259):**
The `if_owner` field in role permissions creates conditional grants:
- If permission is set AND `if_owner=1`: user gets it ONLY if owner.
- If permission is set AND `if_owner=0`: user gets it always (role-based).
- Special: if_owner does NOT give create rights (line 269).

---

## 4. `get_role_permissions(doctype_meta, user, is_owner)` — Role-Based Permission Evaluation

**Function Signature (line 282):**
```python
def get_role_permissions(doctype_meta, user=None, is_owner=None, debug=False):
```

**Flow (lines 282–342):**

```
Input: doctype_meta (DocType metadata), user (str), is_owner (bool optional)
Output: dict — {
            "read": 0|1,
            "write": 0|1,
            ...[all ptypes]...,
            "if_owner": {"read": 0|1, ...},
            "has_if_owner_enabled": 0|1
        }

1. CACHE KEY:
   cache_key = (doctype_meta.name, user, is_owner)
   
   IF cached AND NOT debug:
      RETURN cached_value

2. QUICK CHECKS:
   IF user == "Administrator":
      RETURN allow_everything(doctype)  [all rights = 1]

3. GET USER ROLES:
   roles = frappe.get_roles(user)

4. FILTER APPLICABLE PERMISSIONS:
   applicable_perms = []
   FOR EACH perm IN doctype_meta.permissions:
      IF perm.role IN roles AND perm.permlevel == 0:
         applicable_perms.append(perm)
   
   [Only permlevel 0 (top-level) permissions count for basic access]

5. DETECT IF_OWNER RULES:
   has_if_owner_enabled = ANY(perm.if_owner == 1 FOR perm IN applicable_perms)

6. EVALUATE EACH PERMISSION TYPE:
   FOR EACH ptype IN get_rights(doctype):
      base_value = ANY(perm[ptype] == 1 AND perm.if_owner == 0 FOR perm IN applicable_perms)
      
      IF base_value:
         result[ptype] = 1
      ELSE:
         IF has_if_owner_enabled AND ANY(perm[ptype] == 1 AND perm.if_owner == 1 FOR perm IN applicable_perms):
            IF ptype NOT IN ("select", "read"):
               # Only owner can have this perm
               result[ptype] = 0
               result["if_owner"][ptype] = is_owner ? 1 : 0
            ELSE:
               # select/read are always granted for filtering/list display
               result[ptype] = 1
               result["if_owner"][ptype] = is_owner ? 1 : 0
         ELSE:
            result[ptype] = 0

7. CACHE result

8. RETURN result
```

**Schema Query on tabDocPerm/Custom DocPerm:**
```sql
SELECT * FROM tabDocPerm
WHERE parent = <doctype>
AND permlevel = 0
AND docstatus = 0

Columns of interest:
  - parent: doctype name
  - role: role name
  - permlevel: 0 (basic permissions)
  - read, write, create, submit, cancel, amend, print, email, report, import, export, share: 0|1
  - if_owner: 0|1
```

---

## 5. `has_user_permission(doc, user, ptype)` — User Permission Filtering

**Function Signature (line 351):**
```python
def has_user_permission(doc, user=None, debug=False, *, ptype=None):
    """Return True if User is allowed to view considering User Permissions."""
```

**Flow (lines 351–478):**

```
Input: doc (Document), user (str), ptype (str optional, "read" or "write")
Output: bool — True if user has access via User Permissions OR no rules apply

1. GET USER PERMISSIONS:
   user_perms = get_user_permissions(user)
   IF user_perms is empty:
      RETURN True  ✓ (no restrictions)

2. STRICT MODE CHECK:
   apply_strict = system_settings["apply_strict_user_permissions"]
   
   IF doc.meta.issingle:
      apply_strict = False  [singles have no link field restrictions]
   
   IF apply_strict AND doc.__islocal AND ptype IN ("read", "write"):
      apply_strict = False  [local (unsaved) docs not restricted]

3. STEP 1: CHECK SELF PERMISSION:
   doctype = doc.doctype
   docname = doc.name
   
   IF doctype IN user_perms:
      # User has permission rules for this doctype
      doctype_perms = user_perms[doctype]
      allowed_docs = get_allowed_docs_for_doctype(doctype_perms, doctype)
      
      IF allowed_docs is non-empty:
         IF doc.meta.is_tree AND ptype == "create":
            # For hierarchical trees, allow create if parent is allowed
            FOR EACH parent_ancestor IN [doc.parent, ancestors...]:
               IF parent_ancestor IN allowed_docs AND NOT doc_hide_descendants:
                  not_permitted = False
                  BREAK
         ELSE:
            not_permitted = (NOT docname OR docname NOT IN allowed_docs)
         
         IF not_permitted:
            RETURN False  ✗ (user has no permission for this specific doc)

4. STEP 2: CHECK LINK FIELD RESTRICTIONS:
   FOR EACH link_field IN doc.meta.get_link_fields():
      IF field.ignore_user_permissions:
         CONTINUE  [skip this field]
      
      field_value = doc[field.fieldname]
      IF NOT field_value AND NOT apply_strict:
         CONTINUE  [empty field allowed in non-strict mode]
      
      field_doctype = field.options  [the linked doctype]
      
      IF field_doctype NOT IN user_perms:
         CONTINUE  [no permission rules for this doctype]
      
      # Get allowed values for this link
      allowed_values = get_allowed_docs_for_doctype(
         user_perms[field_doctype],
         doctype  [the parent doctype we're checking]
      )
      
      IF allowed_values is non-empty AND field_value NOT IN allowed_values:
         RETURN False  ✗ (link field value not permitted)

   # Repeat for all child records
   FOR EACH child_doc IN doc.get_all_children():
      IF NOT check_link_fields(child_doc):
         RETURN False  ✗

5. RETURN True  ✓ (passed all checks)
```

**`get_allowed_docs_for_doctype()` Function (line 777):**
```python
def get_allowed_docs_for_doctype(user_permissions, doctype):
    # Filter user_perms where:
    #  - applicable_for is NULL/empty (apply_to_all_doctypes == 1)
    #  OR
    #  - applicable_for == doctype
    
    allowed = []
    FOR EACH perm IN user_permissions:
        IF NOT perm.applicable_for OR perm.applicable_for == doctype:
            allowed.append(perm.for_value)  [the actual doc name]
    
    RETURN allowed
```

**User Permissions Table Schema (tabUser Permission):**
```sql
CREATE TABLE tabUserPermission (
    name VARCHAR PRIMARY KEY,
    user VARCHAR,           -- Link to User
    allow VARCHAR,          -- Doctype name (e.g., "Company", "Customer")
    for_value VARCHAR,      -- Doc name of that doctype
    applicable_for VARCHAR, -- Optional: restrict to specific linked doctype
    apply_to_all_doctypes INT,  -- 0 if applicable_for is set, 1 if applies globally
    is_default INT,         -- 1 if this is default selection
    hide_descendants INT,   -- For tree doctype: 1 to hide children
    ...metadata...
);

Key Indexes:
  - user
  - allow
  - for_value
  - applicable_for
```

**Default Behavior (when user has no permission for a linked doctype):**
- If `apply_strict_user_permissions = 0` (permissive): empty link fields are allowed, only filled ones are checked.
- If `apply_strict_user_permissions = 1` (strict): even empty link fields must be in the allowed list (fail open = false).

---

## 6. DocShare — Per-Document Permission Grants

**Table Schema (tabDocShare):**
```sql
CREATE TABLE tabDocShare (
    name VARCHAR PRIMARY KEY,
    user VARCHAR,          -- NULL if everyone=1
    share_doctype VARCHAR, -- DocType name
    share_name VARCHAR,    -- Document name (DynamicLink)
    read INT,              -- 0|1
    write INT,             -- 0|1
    submit INT,            -- 0|1 (only if doctype.is_submittable)
    share INT,             -- 0|1
    everyone INT,          -- 0|1 if shared with all users (user=NULL)
    notify_by_email INT,   -- 0|1
    ...metadata...
);

Key Indexes:
  - (user, share_doctype)
  - (share_doctype, share_name)
```

**Query to Check Shared Access (`frappe.share.get_shared`):**
```python
def get_shared(doctype, user=None, rights=None, *, filters=None, limit=None):
    """
    rights: List of permission types to check (e.g., ["read", "write"])
    filters: Optional additional filters (e.g., [["share_name", "=", doc_name]])
    """
    
    share_filters = []
    FOR EACH right IN rights:
        share_filters.append([right, "=", 1])
    
    share_filters.append(["share_doctype", "=", doctype])
    
    IF filters:
        share_filters.extend(filters)
    
    or_filters = [
        ["user", "=", user],
        ["everyone", "=", 1]  [only if user != "Guest"]
    ]
    
    SELECT share_name FROM tabDocShare
    WHERE (share_doctype = doctype)
      AND (read=1 OR write=1 OR ...)  [any of requested rights]
      AND (user = <user> OR everyone = 1)
    LIMIT limit
    
    RETURN [share_name, ...]
```

**Cascade Rules (line 43–47):**
```
IF share=1 OR write=1 OR submit=1:
    read = 1  [automatically grant read if any higher perm given]

IF submit=1:
    write = 1  [automatically grant write if submit given]
```

**Sharing Limitations:**
- Only these ptypes can be shared: `["read", "write", "submit", "share", "email", "print"]` + custom rights.
- Cannot share `create`, `cancel`, `amend`, `delete`, `import`, `export`, `report`.
- Cannot share with permissions the sharer doesn't have (validation in `check_share_permission`).

---

## 7. Permission Check Order (Full Picture)

**When `has_permission(doctype, ptype, doc, user)` is called:**

```
1. Administrator? → ALLOW
2. Sharing disabled globally? → DENY
3. Child doctype? → Delegate to parent
4. Get doctype metadata
5. Single doctype without doc? → Treat docname as doctype name

IF doc is provided:
   6a. Lazy load doc if string/int
   6b. get_doc_permissions(doc, user, ptype)
       - check has_controller_permissions()
       - get_role_permissions()
       - if_owner override
       - has_user_permission() restriction
       - user permission link field checks

IF no permission from doc_permissions:
   6c. Check DocShare for specific doc

IF ptype == "select" and still no permission:
   6d. Fall back to "read" permission

Return bool
```

**Controller Permissions (line 481–498):**
```python
def has_controller_permissions(doc, ptype, user=None, debug=False) -> bool:
    # Calls hooks["has_permission"] for doc.doctype
    # Hooks can only DENY (return False), not grant
    
    for method in reversed(hooks.get(doc.doctype, []) + hooks.get("*", [])):
        result = frappe.call(method, doc=doc, ptype=ptype, user=user)
        if NOT result:
            return False  ✗ (veto)
    
    return True  ✓ (no veto)
```

---

## 8. Caching & Performance Notes

**Role Permissions Caching:**
- Key: `(doctype_name, user, is_owner)`
- Cleared on: User role changes, DocPerm/Custom DocPerm changes
- Local to `frappe.local.role_permissions` (request-scoped)

**Role List Caching:**
- Key: `("roles", user)`
- Cached in `frappe.cache.hget("roles", user, ...)`
- Cleared on: User role assignments change

**User Permissions Caching:**
- Key: `("user_permissions", user)` in `frappe.cache`
- Cleared on: User Permission insert/update/delete
- Includes nested set descendants for tree doctypes

---

## Summary Table

| Function | Input | Output | Purpose |
|----------|-------|--------|---------|
| `get_roles(user)` | user email | List[role] | Get user's assigned + automatic roles |
| `has_permission(doctype, ptype, doc, user)` | doctype, ptype, optional doc, user | bool | Master permission check |
| `get_doc_permissions(doc, user)` | doc object, user | dict {ptype: 0\|1} | Evaluate all rights for a specific doc |
| `get_role_permissions(meta, user, is_owner)` | doctype meta, user, owner flag | dict {ptype: 0\|1, if_owner: {...}} | Role-based rights |
| `has_user_permission(doc, user)` | doc object, user | bool | Check User Permission restrictions |
| `get_user_permissions(user)` | user | dict {doctype: [docs]} | Fetch all User Permission rules |
| `get_shared(doctype, user, rights)` | doctype, user, [rights] | List[doc_names] | Get docs shared via DocShare |
