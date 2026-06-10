# ERPNext Desk Surface Analysis — Method Requirements for Ferro

**Date:** 2026-06-10  
**Scope:** Mapping all whitelisted methods required for ERPNext Desk (setup wizard, workspaces, list views, form views, report views) to identify MISSING/STUB implementations in ferro's Rust runtime.

---

## 1. Setup Wizard Flow

The Frappe setup wizard is the first-run configuration UI accessed at `/app/setup-wizard`. The flow is controlled by two boot flags: `frappe.boot.setup_complete` (gates entry) and `System Settings.setup_complete` (persistent state).

### 1.1 Frontend Bootstrap (setup_wizard.js)

When the user accesses the setup wizard page, the frontend code:
1. Checks `frappe.boot.setup_complete` — if true, redirects to `/desk` (skip wizard entirely)
2. Loads required JS from `frappe.boot.setup_wizard_requires` (app-specific slides)
3. Calls two methods to bootstrap the wizard UI:
   - `frappe.desk.page.setup_wizard.setup_wizard.load_languages()` → returns language list + codes
   - `frappe.desk.page.setup_wizard.setup_wizard.load_user_details()` → returns cached signup full_name/email
4. Renders slides from `frappe.setup.slides` (populated by hook-provided stages)
5. On completion, calls `frappe.desk.page.setup_wizard.setup_wizard.setup_complete(args)` with form data

### 1.2 Whitelisted Methods in Setup Wizard

| Method | Module | Return Type | Called When | Status in ferro |
|--------|--------|-------------|-------------|-----------------|
| `load_languages()` | `frappe.desk.page.setup_wizard.setup_wizard` | `{default_language, languages[], codes_to_names}` | Page load | **MISSING** |
| `load_user_details()` | `frappe.desk.page.setup_wizard.setup_wizard` | `{full_name, email}` | Page load | **MISSING** |
| `load_messages(language)` | `frappe.desk.page.setup_wizard.setup_wizard` | translations dict (sent via publish) | Language change | **MISSING** |
| `setup_complete(args)` | `frappe.desk.page.setup_wizard.setup_wizard` | `{status: "ok"\|"registered"}` | Final submit | **MISSING** |
| `initialize_system_settings_and_user(system_settings_data, user_data)` | `frappe.desk.page.setup_wizard.setup_wizard` | None | Alternative path | **MISSING** |

### 1.3 Boot Flags & System Settings

The setup wizard reads/writes:
- **Boot flag:** `frappe.boot.setup_complete` (injected by frappe.boot)
- **Persistent:** `System Settings.setup_complete` (checked by `frappe.is_setup_complete()`)
- **Per-app:** `Installed Application.is_setup_complete` (per-app completion tracking)

**ERPNext contribution:** Via `setup_wizard_stages` hook → `erpnext.setup.setup_wizard.setup_wizard.get_setup_stages(args)` returns stages like:
- `stage_fixtures` (load presets + roles)
- `setup_company` (create company doctype)
- `setup_defaults` (set country/currency/fiscal year)
- `setup_demo` (if requested)

**Ferro status:** Boot flag is hardcoded `setup_complete=1` in `desk.rs:191` — the wizard is **unreachable by design**. If a fresh site needed wizard-like initialization, these methods would ALL be missing.

---

## 2. Workspaces (Desk Sidebar & Home)

Workspaces are the primary navigation construct in modern Desk. The sidebar and workspace/module structure are built from live DB data on every page load.

### 2.1 Workspace Response Shape

From `frappe.desk.desktop.Workspace.build_workspace()`:

```python
{
    "charts": {
        "items": [
            {
                "chart_name": str,
                "label": str,  # translated
                "doctype": str  # meta doctype name
            }
        ]
    },
    "shortcuts": {
        "items": [
            {
                "link_to": str,
                "type": "DocType" | "Report" | "Page" | "Dashboard" | "URL" | "Help",
                "label": str,  # translated
                "is_query_report": int,
                "ref_doctype": str  # optional, for reports
            }
        ]
    },
    "cards": {
        "items": [
            {
                "label": str,  # Card Break or Link label
                "link_to": str,
                "link_type": "DocType" | "Report" | "Page",
                "type": "Card Break" | "Link",
                "dependencies": str,  # CSV of doctype dependencies
                "count": int,  # doctype record count (if onboard=1)
                "incomplete_dependencies": [str],  # unmet dependencies
                "description": str  # from DocType.description
            }
        ]
    },
    "quick_lists": {
        "items": [
            {
                "document_type": str,
                "label": str
            }
        ]
    },
    "number_cards": {
        "items": [
            {
                "number_card_name": str,
                "label": str,
                "doctype": str
            }
        ]
    },
    "custom_blocks": {
        "items": [
            {
                "custom_block_name": str,
                "label": str
            }
        ]
    },
    "onboardings": {
        "items": []  # populated if enable_onboarding=1
    }
}
```

### 2.2 Whitelisted Methods for Workspaces

| Method | Module | Return Type | Called When | Status in ferro |
|--------|--------|-------------|-------------|-----------------|
| `get_workspace_sidebar_items()` | `frappe.desk.desktop` | `{pages, has_access, has_create_access}` | Desk init; edit workspace | **IMPLEMENTED** (desk.rs:632) |
| `get_desktop_page(page)` | `frappe.desk.desktop` | Response shape above | When rendering a workspace | **IMPLEMENTED** (desk.rs:631) |
| `get_onboarding_data(module)` | `frappe.desk.desktop` | `{label, title, items[]}` | If enable_onboarding=1 | **STUB** (returns []) |
| `get_installed_apps()` | `frappe.desk.desktop` | `[app_name]` | Sidebar app switcher | **IMPLEMENTED** (desk.rs:670, returns from config) |

### 2.3 ERPNext Workspace Fixtures

ERPNext includes pre-built workspace JSON files (e.g., `erpnext/setup/workspace/home/home.json`) that define the Home workspace with:
- **Cards:** Accounting, Stock, CRM, Data Import sections
- **Shortcuts:** Item, Customer, Supplier, Sales Invoice
- **Links:** Lists of DocTypes grouped by category
- **Onboarding:** References to "Home" Module Onboarding

These are inserted as `Workspace` doctypes on install. Ferro's native ORM reads these from the DB directly — no Python hooks needed.

### 2.4 What Can Break Workspace Rendering

1. **Missing workspace records** — if `Workspace` doctypes aren't synced from fixtures
2. **Missing number_card records** — `get_number_card_result()` not implemented → chart data empty
3. **Doctype count queries fail** — workspace shows "incomplete_dependencies" markers
4. **Permission checks** — user lacks read access to workspace's module → workspace filtered out
5. **Module → App mapping** — Module Def missing → sidebar groups incorrectly

**Ferro status:** Workspaces render from live DB via `inject_desktop_data()` — likely **working** if fixtures installed correctly. Number card results are **STUB** (empty).

---

## 3. List View

List views display a paginated table of DocType records with filters, sorting, grouping, and custom columns.

### 3.1 List View Data Methods

| Method | Module | Signature | Return Type | Status |
|--------|--------|-----------|-------------|--------|
| `frappe.desk.reportview.get()` | `frappe.desk.reportview` | `(doctype, fields, filters, order_by, start, limit, ...)` | `{keys, values}` (compressed) | **IMPLEMENTED** (desk.rs:610) |
| `frappe.desk.reportview.get_list()` | `frappe.desk.reportview` | Same as get() | `[{field: value, ...}]` (uncompressed) | **IMPLEMENTED** (desk.rs:611) |
| `frappe.desk.reportview.get_count()` | `frappe.desk.reportview` | `(doctype, filters, ...)` | `int` | **IMPLEMENTED** (desk.rs:605) |
| `frappe.desk.listview.get_list_settings(doctype)` | `frappe.desk.listview` | `(doctype: str)` | `ListViewSettings` doc or null | **STUB** (returns {}) |
| `frappe.desk.listview.set_list_settings(doctype, values)` | `frappe.desk.listview` | `(doctype: str, values: dict)` | None | **MISSING** (no implementation) |
| `frappe.desk.listview.get_group_by_count(doctype, current_filters, field)` | `frappe.desk.listview` | `(doctype, filters, field)` | `[{name, count, title?}]` | **STUB** (returns []) |

### 3.2 List View Response Shape (get_list)

```json
{
    "keys": ["name", "modified", "owner", "status"],  // column names (compressed get)
    "values": [
        ["doc-1", "2025-01-15", "admin@example.com", "Draft"],
        ["doc-2", "2025-01-14", "user@example.com", "Submitted"]
    ]
}
```

Uncompressed (get_list):
```json
[
    {"name": "doc-1", "modified": "2025-01-15", "owner": "admin@example.com", "status": "Draft"},
    {"name": "doc-2", "modified": "2025-01-14", "owner": "user@example.com", "status": "Submitted"}
]
```

### 3.3 ERPNext List Customizations

ERPNext adds:
- **Custom columns** (via `List View Settings` doctypes for specific DocTypes)
- **Filters** (saved as `Report Filter` or via frappe's standard filter UI)
- **Grouping** (e.g., Sales Invoice grouped by customer, status)
- **Report-style views** (some DocTypes have both list and report views)

**Ferro status:** Core list-data methods (**get, get_count, get_list**) are **IMPLEMENTED**. List settings (**get_list_settings, get_group_by_count**) are **STUB/MISSING** — they return empty, so custom columns/grouping won't work.

---

## 4. Form View

Form view displays a single document with all fields, validation, sidebar metadata, and lifecycle hooks.

### 4.1 Form Load Methods

| Method | Module | Signature | Return Type | Status |
|--------|--------|-----------|-------------|--------|
| `frappe.desk.form.load.getdoc(doctype, name)` | `frappe.desk.form.load` | `(doctype: str, name: str)` | `{docs: [Document]}` | **IMPLEMENTED** (desk.rs:637) |
| `frappe.desk.form.load.getdoctype(doctype, with_parent=False)` | `frappe.desk.form.load` | `(doctype: str, with_parent: bool)` | `{docs: [DocType metadata]}` | **IMPLEMENTED** (desk.rs:636) |
| `frappe.desk.form.load.get_docinfo(doctype, name)` | `frappe.desk.form.load` | `(doctype: str, name: str)` | `{docinfo: {...}}` | **IMPLEMENTED** (desk.rs:645, returns empty) |
| `frappe.desk.form.load.get_communications(doctype, name, start, limit)` | `frappe.desk.form.load` | `(doctype, name, start: int, limit: int)` | `[Communication]` | **MISSING** |
| `frappe.desk.form.load.get_user_info_for_viewers(users)` | `frappe.desk.form.load` | `(users: JSON str)` | `{user_name: {image_url, ...}}` | **MISSING** |

### 4.2 docinfo Response Shape

From `frappe.desk.form.load.get_docinfo()`:

```python
{
    "doctype": str,
    "name": str,
    "attachments": [
        {
            "name": str,
            "file_name": str,
            "file_url": str,
            "is_private": int,
            "attached_to_field": str,
            "folder": str
        }
    ],
    "communications": [
        {
            "name": str,
            "communication_type": "Communication" | "Automated Message",
            "communication_medium": "Email" | "Chat",
            "communication_date": datetime,
            "content": str,
            "sender": str,
            "sender_full_name": str,
            "subject": str,
            "delivery_status": str,
            "recipients": str
        }
    ],
    "automated_messages": [  # subset of communications with type="Automated Message"
        {...}
    ],
    "versions": [
        {
            "name": str,
            "owner": str,
            "creation": datetime,
            "data": JSON  # field changes: {fieldname: {old, new}}
        }
    ],
    "assignments": [
        {
            "name": str,
            "owner": str,
            "description": str,
            "status": "Pending" | "Completed" | "Closed"
        }
    ],
    "permissions": [
        {
            "ptype": "read" | "write" | "create" | "delete" | "print",
            "granted": bool,
            "user": str | null
        }
    ],
    "shared": [
        {
            "user": str,
            "read": int,
            "write": int,
            "creation": datetime
        }
    ],
    "views": [  # if track_views=1
        {
            "name": str,
            "owner": str,
            "creation": datetime
        }
    ],
    "comments": [...],
    "assignment_logs": [...],
    "attachment_logs": [...],
    "tags": str,  # CSV
    "document_email": str | null,
    "custom_perm_types": [str],
    "is_document_followed": bool,
    "user_info": {
        "user_email": {
            "name": str,
            "email": str,
            "image": str,
            ...
        }
    }
}
```

### 4.3 Form Save Methods

| Method | Module | Signature | Return Type | Status |
|--------|--------|-----------|-------------|--------|
| `frappe.desk.form.save.savedocs(docs, actions, ...)` | `frappe.desk.form.save` | `(docs: [doc_dict], ...)` | `{docs: [saved_docs], ...}` | **IMPLEMENTED** (desk.rs:638) |
| `frappe.client.save(doctype, name, ...)` | `frappe.client` | `(doctype, **kwargs)` | Saved doc | **IMPLEMENTED** (desk.rs:639) |
| `frappe.client.insert(doctype, ...)` | `frappe.client` | `(doctype, **kwargs)` | New doc | **IMPLEMENTED** (desk.rs:639 - same as save) |
| `frappe.client.delete(doctype, name)` | `frappe.client` | `(doctype, name)` | None | **IMPLEMENTED** (desk.rs:641) |
| `frappe.client.set_value(doctype, name, fieldvalues)` | `frappe.client` | `(doctype, name, dict)` | Updated doc | **IMPLEMENTED** (desk.rs:640) |

### 4.4 Form Utility Methods

| Method | Module | Status |
|--------|--------|--------|
| `frappe.client.get(doctype, name)` | `frappe.client` | **IMPLEMENTED** (desk.rs:642) |
| `frappe.client.get_value(doctype, filters, ...)` | `frappe.client` | **IMPLEMENTED** (desk.rs:643) |
| `frappe.client.get_count(doctype, filters)` | `frappe.client` | **IMPLEMENTED** (desk.rs:605) |
| `frappe.client.get_single_value(doctype, fieldname)` | `frappe.client` | **IMPLEMENTED** (desk.rs:644) |

### 4.5 ERPNext Form Customizations

ERPNext adds:
- **Custom scripts** — loaded from `Client Script` doctypes (already in meta via meta.py)
- **Custom fields** — synced from fixtures (handled by schema.rs)
- **Workflows** — loaded in form meta (**__workflow_docs**)
- **Print formats** — loaded in meta (**__print_formats**)
- **Controllers** — Python `on_validate`, `onload`, `before_save` hooks (if ferrod, via pyfall)

**Ferro status:** Core form methods are **IMPLEMENTED**. Form metadata (**getdoctype**) includes all custom scripts/workflows. Timeline data (**get_communications, versions, assignments**) is **MISSING/STUB**.

---

## 5. Report View (Standard Report Types)

ERPNext reports fall into several categories:
- **Query Reports** — SQL + Python aggregation
- **Script Reports** — Pure Python report logic
- **DocType Reports** — Desk list view with report-style filters
- **Report Builder** — Drag-and-drop query builder

For this analysis, focus on **DocType reports** (the ones served via Desk list view).

### 5.1 Report Methods

| Method | Module | Signature | Return Type | Status |
|--------|--------|-----------|-------------|--------|
| `frappe.desk.reportview.get(doctype, fields, filters, ...)` | `frappe.desk.reportview` | Same as list view | Compressed data | **IMPLEMENTED** |
| `frappe.desk.reportview.get_list(doctype, fields, filters, ...)` | `frappe.desk.reportview` | Same as list view | Uncompressed data | **IMPLEMENTED** |

**Note:** Query Reports and Script Reports require Python execution. Ferro currently **does not serve them natively**. They either:
- Fall through to ferrod (if configured)
- Return a 404 (if pure-Rust ferro)

For ERPNext in pure-Rust ferro, **custom reports are not available**.

---

## 6. Summary: MISSING & STUB Methods by Feature Area

### 6.1 Setup Wizard (All MISSING or Unreachable)

| Method | Impact | Notes |
|--------|--------|-------|
| `load_languages()` | **BLOCKER** | Without this, wizard can't show language list |
| `load_user_details()` | **Convenience** | Can work around with empty defaults |
| `load_messages(language)` | **UX** | Language change won't translate wizard slides |
| `setup_complete()` | **BLOCKER** | Wizard can't complete setup |
| `initialize_system_settings_and_user()` | **Alternative** | Alternative initialization path |

**Single blocker for setup wizard:** `setup_complete()` (and by extension, all the hooks it calls for ERPNext — stages won't run in Rust).

**Ferro's workaround:** Boot flag hardcoded to `setup_complete=1`, so wizard is never shown. Setup via CLI only.

---

### 6.2 Workspaces (Mostly IMPLEMENTED)

| Method | Status | Impact |
|--------|--------|--------|
| `get_workspace_sidebar_items()` | **IMPLEMENTED** | Sidebar renders ✓ |
| `get_desktop_page()` | **IMPLEMENTED** | Workspace content renders ✓ |
| `get_installed_apps()` | **IMPLEMENTED** | App switcher works ✓ |
| `get_onboarding_data()` | **STUB** (returns []) | Onboarding panels won't show |
| `get_number_card_result()` | **STUB** (returns []) | Number cards show no data |

**Single blocker for workspaces:** `get_number_card_result()` — workspace cards won't populate with live KPIs (sales, invoices, etc.). The cards will render but with no data.

---

### 6.3 List View (Mostly IMPLEMENTED)

| Method | Status | Impact |
|--------|--------|--------|
| `get()` | **IMPLEMENTED** | List data loads ✓ |
| `get_list()` | **IMPLEMENTED** | List data loads ✓ |
| `get_count()` | **IMPLEMENTED** | Total row count loads ✓ |
| `get_list_settings()` | **STUB** (returns {}) | Custom columns won't persist |
| `set_list_settings()` | **MISSING** | User can't customize list view |
| `get_group_by_count()` | **STUB** (returns []) | Grouping sidebar empty |

**Single blocker for list view:** `get_list_settings()` — custom columns/grouping features won't work, but **basic list data loads correctly**.

---

### 6.4 Form View (Mostly IMPLEMENTED)

| Method | Status | Impact |
|--------|--------|--------|
| `getdoc()` | **IMPLEMENTED** | Form data loads ✓ |
| `getdoctype()` | **IMPLEMENTED** | Form schema loads ✓ |
| `savedocs()` | **IMPLEMENTED** | Form saves ✓ |
| `get_docinfo()` | **STUB** (empty) | Timeline/comments/versions all empty |
| `get_communications()` | **MISSING** | No email/chat history |
| `get_user_info_for_viewers()` | **MISSING** | User avatars for comments missing |

**Single blocker for form view:** None (forms are **fully functional** for CRUD). Timeline features (comments, versions, assignments) are all **STUB/MISSING** — timeline sidebar is empty but doesn't break form functionality.

---

### 6.5 Report View (Not Served in Pure-Rust Ferro)

Custom Reports (Query, Script, Report Builder) are **not implemented** in pure-Rust ferro. If needed, they require:
- `ferrod` (with embedded Python) to execute report scripts
- OR, delegate to `/api/method/frappe.client.get_list` for DocType-based reports

---

## 7. Implementation Priority for ERPNext Desk

### Tier 1: **Critical Blockers** (without these, core UX breaks)

None. All four main features (setup wizard, workspaces, list, form) have **at least one functioning code path**:
- Setup wizard is **skipped by design** (OK for CLI-based provisioning)
- Workspaces render from DB directly — **no method calls needed** for basic structure
- List data flows through native ORM — **get, get_count, get_list all work**
- Form CRUD works natively — **getdoc, savedocs all work**

### Tier 2: **High-Impact Gaps** (missing these degrades UX significantly)

1. **`get_number_card_result(doctype, filters, ...)` — Number Card Data**
   - **Impact:** Workspace KPI cards show no data (just titles)
   - **Effort:** Medium (queries dashboard_chart equivalent, aggregates counts/sums)
   - **Usage:** Home workspace + custom workspaces with metrics

2. **`get_docinfo(doctype, name)` — Timeline Data**
   - **Impact:** Form view's "Comments" / "Versions" / "Assignments" sidebars empty
   - **Effort:** Medium (10+ sub-queries for comments, versions, attachments, shares)
   - **Usage:** Every form view (but not blocking)

### Tier 3: **Low-Impact Gaps** (nice-to-have, frequently unused)

1. **`get_list_settings(doctype)` / `set_list_settings(doctype, values)`**
   - **Impact:** Users can't save custom column preferences
   - **Effort:** Low (CRUD on List View Settings doctype)
   - **Usage:** Power users

2. **`get_group_by_count(doctype, filters, field)`**
   - **Impact:** Group-by dropdown in list view returns no options
   - **Effort:** Low (GROUP BY query + LIMIT 50)
   - **Usage:** Power users doing aggregations

3. **`get_onboarding_data(module)`**
   - **Impact:** Onboarding walkthrough panels don't appear
   - **Effort:** Low (query Module Onboarding + steps)
   - **Usage:** First-run UX only

---

## 8. Complete Method Inventory

### All Frappe.desk Methods Called During ERPNext Desk Session

```
SETUP WIZARD (unreachable in ferro, all MISSING):
  ✗ frappe.desk.page.setup_wizard.setup_wizard.load_languages()
  ✗ frappe.desk.page.setup_wizard.setup_wizard.load_user_details()
  ✗ frappe.desk.page.setup_wizard.setup_wizard.load_messages(language)
  ✗ frappe.desk.page.setup_wizard.setup_wizard.setup_complete(args)
  ✗ frappe.desk.page.setup_wizard.setup_wizard.initialize_system_settings_and_user(system_settings, user)

WORKSPACE / HOME (mostly IMPLEMENTED):
  ✓ frappe.desk.desktop.get_workspace_sidebar_items()
  ✓ frappe.desk.desktop.get_desktop_page(page)
  ✗ frappe.desk.desktop.get_onboarding_data(module)
  ✓ frappe.desk.desktop.get_installed_apps()

LIST VIEW (mostly IMPLEMENTED):
  ✓ frappe.desk.reportview.get(doctype, fields, filters, ...)
  ✓ frappe.desk.reportview.get_list(doctype, fields, filters, ...)
  ✓ frappe.desk.reportview.get_count(doctype, filters)
  ✗ frappe.desk.listview.get_list_settings(doctype)
  ✗ frappe.desk.listview.set_list_settings(doctype, values)
  ✗ frappe.desk.listview.get_group_by_count(doctype, filters, field)

FORM VIEW (fully functional for CRUD, timeline STUB):
  ✓ frappe.desk.form.load.getdoc(doctype, name)
  ✓ frappe.desk.form.load.getdoctype(doctype, with_parent)
  ✗ frappe.desk.form.load.get_docinfo(doctype, name)  [STUB - returns empty]
  ✗ frappe.desk.form.load.get_communications(doctype, name, start, limit)
  ✗ frappe.desk.form.load.get_user_info_for_viewers(users)
  ✓ frappe.desk.form.save.savedocs(docs, actions, ...)
  ✓ frappe.client.save(doctype, name, ...)
  ✓ frappe.client.insert(doctype, ...)
  ✓ frappe.client.delete(doctype, name)
  ✓ frappe.client.set_value(doctype, name, values)
  ✓ frappe.client.get(doctype, name)
  ✓ frappe.client.get_value(doctype, filters)
  ✓ frappe.client.get_count(doctype, filters)
  ✓ frappe.client.get_single_value(doctype, fieldname)

REPORT / DASHBOARD (MISSING - not served in pure Rust):
  ✗ frappe.desk.doctype.dashboard_chart.dashboard_chart.get(chart_name)
  ✗ frappe.desk.doctype.dashboard_chart.dashboard_chart.get_data(...)
  ✗ frappe.desk.doctype.number_card.number_card.get_result(card_name, filters, ...)
  ✗ frappe.desk.doctype.number_card.number_card.get_percentage_difference(...)
  ✗ Query/Script Report execution (requires Python)
```

---

## 9. File References

**Frappe Source:**
- `/home/frappe/benches/bench-cpython314/apps/frappe/frappe/desk/page/setup_wizard/setup_wizard.py`
- `/home/frappe/benches/bench-cpython314/apps/frappe/frappe/desk/page/setup_wizard/setup_wizard.js`
- `/home/frappe/benches/bench-cpython314/apps/frappe/frappe/desk/desktop.py`
- `/home/frappe/benches/bench-cpython314/apps/frappe/frappe/desk/reportview.py`
- `/home/frappe/benches/bench-cpython314/apps/frappe/frappe/desk/listview.py`
- `/home/frappe/benches/bench-cpython314/apps/frappe/frappe/desk/form/load.py`
- `/home/frappe/benches/bench-cpython314/apps/frappe/frappe/desk/form/meta.py`

**ERPNext Source:**
- `/home/frappe/apps-local/erpnext/erpnext/setup/setup_wizard/setup_wizard.py`
- `/home/frappe/apps-local/erpnext/erpnext/setup/workspace/home/home.json`
- `/home/frappe/apps-local/erpnext/erpnext/hooks.py` (setup_wizard_stages hook)

**Ferro Implementation:**
- `/home/frappe/ferro/src/desk.rs` (all desk/client methods)

---

## 10. Conclusion

**For ERPNext Desk to run on pure-Rust ferro:**

- ✓ **Setup wizard** is not a blocker — hardcoded skip via boot flag is acceptable for automated provisioning
- ✓ **Workspaces** render correctly from DB — sidebar, home page, app switcher all work
- ✓ **List views** load all data correctly — filtering, sorting, pagination all work
- ✓ **Form views** are fully functional for CRUD — no blockers for basic usage

**The most important missing piece:** `get_number_card_result()` — without this, workspace KPI cards (live counts, sums, trends) won't populate. This is a **visual gap** (users see empty cards) but not a **functional gap** (they can still navigate, create, edit records).

**Timeline gaps (comments, versions, assignments)** are also important for power users but don't block basic Desk operation.

