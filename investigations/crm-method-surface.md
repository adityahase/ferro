# CRM Frontend Method Surface Map

**Generated:** 2026-06-10  
**Frontend:** Frappe CRM SPA (built Vue3 + frappe-ui)  
**Scope:** All server methods the CRM frontend calls via `/api/method/*` endpoints

---

## Summary

- **Total Methods:** 57 distinct method paths
- **CRM API Methods:** 44 (`crm.api.*`)
- **Frappe Client Methods:** 6 (`frappe.client.*`)
- **Frappe Desk Methods:** 5 (`frappe.desk.*`)
- **Exchange Rate Utilities:** 1 (`crm.api.exchange_rate.*`)

**CONTROLLER-dependent methods (business logic, app methods):**
- `crm.api.doc.get_data` → `default_list_data()`, `parse_list_data()` (CRM Lead/Deal controllers)
- `crm.api.dashboard.get_dashboard` → 20+ custom aggregation methods
- `crm.api.dashboard.get_chart` → per-chart method dispatch
- `crm.api.session.get_users` → role filtering (CRM_ALLOWED_ROLES)

---

## CRM API Methods (`crm.api.*`)

### Doc Operations

| Method | Returns | Implementability | Notes |
|--------|---------|------------------|-------|
| **get_data** | `{data[], columns[], rows[], fields[], kanban_columns?, kanban_fields?, group_by_field?, page_length, page_length_count, is_default, views[], total_count, row_count, form_script, list_script, view_type}` | **CONTROLLER** | Calls `_list.default_list_data()` for Kanban/custom views; `parse_list_data()` for post-processing |
| **get_filterable_fields** | `[{fieldname, fieldtype, label, name, value, options}]` | **META** | Reads doctype meta + controller `get_non_filterable_fields()` hook |
| **get_quick_filters** | `[{label, fieldname, fieldtype, options}]` | **META** | Reads meta + CRM Global Settings; handles Select option parsing |
| **get_group_by_fields** | `[{label, fieldname}]` | **META** | Reads doctype meta, filters by allowed fieldtypes |
| **sort_options** | `[{label, value, fieldname}]` | **META** | Lists all sortable fields + 5 standard (name, creation, modified, modified_by, owner) |
| **get_fields** | `[{fieldname, fieldtype, label, ...}]` (DocField objects) | **META** | Returns all non-readonly DocFields for a doctype |
| **get_assigned_users** | `[user_names]` | **CRUD** | Queries ToDo table for reference_type=doctype, status != Cancelled |
| **get_linked_docs_of_document** | `[{doc, title, reference_docname, reference_doctype}]` | **CONTROLLER** | Uses `get_linked_docs()` + `get_dynamic_linked_docs()` frappe internals; handles CRM special cases |
| **remove_assignments** | `null` | **CRUD** | Removes ToDo assignments via `set_status(..., status="Cancelled")` |
| **remove_linked_doc_reference** | `"success"` | **CONTROLLER** | Unlinks or deletes linked docs (Contact/notification cleanup logic) |
| **delete_bulk_docs** | `"success"` | **CONTROLLER** | Queries linked docs; delegates to `frappe.desk.reportview.delete_bulk` for async |
| **update_quick_filters** | `null` | **CRUD** | Updates CRM Global Settings + Property Setters for in_standard_filter |

### Contact Operations

| Method | Returns | Implementability | Notes |
|--------|---------|------------------|-------|
| **contact.get_linked_deals** | `[{name, organization, currency, annual_revenue, status, email, mobile_no, deal_owner, modified}]` | **CRUD** | Joins Contact → CRM Contacts → CRM Deal |
| **contact.create_new** | `true` | **CRUD** | Appends email_ids/phone_nos to Contact; handles primary flag |
| **contact.set_as_primary** | `true` | **CRUD** | Updates is_primary/is_primary_mobile_no flags on Contact subtables |
| **contact.search_emails** | `[[full_name, email_id, name], ...]` | **CRUD** | Text search on Contact with email_id filter |

### Activity Operations

| Method | Returns | Implementability | Notes |
|--------|---------|------------------|-------|
| **activities.get_activities** | `([activities], [calls], [notes], [tasks], [attachments])` | **CONTROLLER** | Reads docinfo versions (via frappe internals); groups version changes; handles Deal→Lead conversion history |

### Notification Operations

| Method | Returns | Implementability | Notes |
|--------|---------|------------------|-------|
| **notifications.get_notifications** | `[{creation, from_user{name, full_name}, type, to_user, read, hash, notification_text, notification_type_doctype, notification_type_doc, reference_doctype, reference_name, route_name}]` | **CRUD** | Queries CRM Notification; maps reference_doctype to UI names (Deal/Lead) |
| **notifications.mark_as_read** | `null` | **CRUD** | Updates CRM Notification.read=True per user/doc filter |

### Session/User Operations

| Method | Returns | Implementability | Notes |
|--------|---------|------------------|-------|
| **session.get_users** | `(all_users[], crm_users[])` | **CONTROLLER** | Filters by CRM_ALLOWED_ROLES; maps roles per user; checks telephony agent flag |
| **session.get_organizations** | `[{...org fields...}]` | **CRUD** | Queries all CRM Organization records |

### View Management

| Method | Returns | Implementability | Notes |
|--------|---------|------------------|-------|
| **views.get_views** | `[{...view fields...}]` | **CRUD** | Queries CRM View Settings; filters by user="" or user=session.user |

### Dashboard

| Method | Returns | Implementability | Notes |
|--------|---------|------------------|-------|
| **dashboard.get_dashboard** | `[{name, data}]` (layout with per-chart data) | **CONTROLLER** | Loads CRM Dashboard doc; dispatches to `get_<name>()` methods per chart; 20+ helper methods for aggregation |
| **dashboard.get_chart** | Depends on chart type | **CONTROLLER** | Maps chart name → method, executes it |
| **dashboard.reset_to_default** | `null` | **CONTROLLER** | Calls `create_default_manager_dashboard(force=True)` |

Dashboard helper methods (all **CONTROLLER**, use frappe Query Builder + aggregations):
- `get_total_leads` → count with delta % vs prev period
- `get_ongoing_deals` → filter by Status.type not in ["Won", "Lost"]
- `get_average_ongoing_deal_value` → avg(deal_value * exchange_rate)
- `get_won_deals` → filter by closed_date, Status.type == "Won"
- `get_average_won_deal_value` → avg for won deals
- `get_average_deal_value` → avg all non-lost
- `get_average_time_to_close_a_lead` → TIMESTAMPDIFF(DAY, Lead.creation, Deal.closed_date)
- `get_average_time_to_close_a_deal` → same, from Deal creation
- `get_sales_trend` → union leads/deals by date
- `get_forecasted_revenue` → expected_deal_value * probability / 100 (vs actual Won)
- `get_funnel_conversion` → stage counts (Leads, then each Deal Status)
- `get_deals_by_stage_axis` → count per status
- `get_deals_by_stage_donut` → count per status (grouped)
- `get_lost_deal_reasons` → count per lost_reason
- `get_leads_by_source` → count per source
- `get_deals_by_source` → count per source
- `get_deals_by_territory` → count + sum(deal_value) per territory
- `get_deals_by_salesperson` → count + sum per deal_owner

### Comment/Activity

| Method | Returns | Implementability | Notes |
|--------|---------|------------------|-------|
| **comment.add_comment** | `{name, ...comment fields...}` | **CRUD** | Wraps `frappe.desk.form.utils.add_comment()`; handles mentions (extracts @mentions, sends notifications) |

### Assignment Rules

| Method | Returns | Implementability | Notes |
|--------|---------|------------------|-------|
| **assignment_rule.get_assignment_rules_list** | `[{name, description, disabled, priority, users_exists}]` | **CRUD** | Queries Assignment Rule for CRM Lead/Deal; checks if users exist |
| **assignment_rule.duplicate_assignment_rule** | `{...doc...}` | **CRUD** | Clones Assignment Rule with new name |

### WhatsApp Operations

| Method | Returns | Implementability | Notes |
|--------|---------|------------------|-------|
| **whatsapp.is_whatsapp_enabled** | `bool` | **TRIVIAL** | Checks WhatsApp Settings existence + active account |
| **whatsapp.is_whatsapp_installed** | `bool` | **TRIVIAL** | Checks if WhatsApp Settings doctype exists |
| **whatsapp.get_whatsapp_messages** | `[{name, type, to, from, content_type, message_type, ..., template?, header?, footer?, reaction?, ...}]` | **CONTROLLER** | Loads messages; enriches with template data; handles reply/reaction chains |
| **whatsapp.create_whatsapp_message** | `doc_name` | **CRUD** | Creates WhatsApp Message with reply linkage |
| **whatsapp.send_whatsapp_template** | `doc_name` | **CRUD** | Creates template-type WhatsApp Message |
| **whatsapp.react_on_whatsapp_message** | `doc_name` | **CRUD** | Creates reaction message (content_type=reaction) |

### Auth/User Management

| Method | Returns | Implementability | Notes |
|--------|---------|------------------|-------|
| **user.change_password** | `"Password Updated Successfully"` | **CRUD** | Validates old password; enforces strength; calls frappe password update |
| **user.add_existing_users** | `null` | **CONTROLLER** | Loops `update_user_role(user, role)` |
| **user.update_user_role** | `null` | **CONTROLLER** | Appends roles to User doc; handles role hierarchy (System Manager → Sales Manager → Sales User) |
| **user.remove_crm_roles_from_user** | `"User ... has been removed from CRM roles."` | **CONTROLLER** | Removes CRM roles; handles Sales Hierarchy cleanup |

### Settings/Exchange Rate

| Method | Returns | Implementability | Notes |
|--------|---------|------------------|-------|
| **settings.create_email_account** | `"Service not supported"` or `null` (on success) | **CRUD** | Creates Email Account doc; validates IMAP/SMTP credentials per provider config |
| **exchange_rate.get_exchange_rate** | `float` (rate) | **CONTROLLER** | Multi-provider fallback (exchangerate.host, exchangerate-api, frankfurter, fawaz); caches per date |

### Onboarding

| Method | Returns | Implementability | Notes |
|--------|---------|------------------|-------|
| **onboarding.get_first_lead** | `name` or `null` | **CRUD** | Returns first unconverted CRM Lead by creation |
| **onboarding.get_first_deal** | `name` or `null` | **CRUD** | Returns first CRM Deal by creation |

### Misc

| Method | Returns | Implementability | Notes |
|--------|---------|------------------|-------|
| **delete_attachment** | `null` | **CRUD** | Deletes File record by file_url + doctype/docname |
| **get_file_uploader_defaults** | `{allowed_file_types, max_file_size, max_number_of_files?, make_attachments_public?}` | **META** | Reads system settings + doctype meta |
| **get_user_signature** | `<div>...HTML...</div>` or `null` | **CRUD** | Returns User.email_signature or default Email Account signature |
| **invite_by_email** | `{existing_members[], existing_invites[], to_invite[]}` | **CRUD** | Creates CRM Invitation records for email list |

---

## Frappe Client Methods (`frappe.client.*`)

| Method | Returns | Implementability | Notes |
|--------|---------|------------------|-------|
| **get** | Full doc object | **CRUD** | Fetch single doc by doctype + name |
| **get_list** | `[{fields...}]` | **CRUD** | List with filters, order_by, limit |
| **get_list_count** | Implied via get_list | **CRUD** | Counted separately in CRM via COUNT aggregate |
| **insert** | `{name, ...fields...}` | **CRUD** | Create new doc |
| **set_value** | `{name, ...updated_fields...}` | **CRUD** | Update single field(s) |
| **rename_doc** | `{new_name, ...}` | **CRUD** | Rename a doc |
| **delete** | `null` | **CRUD** | Delete doc |
| **get_doc_permissions** | `{read, write, create, delete, ...perms}` | **META** | Check permissions on doc |
| **get_single_value** | Scalar value | **CRUD** | Get single field from a Single doctype |

---

## Frappe Desk Methods (`frappe.desk.*`)

| Method | Returns | Implementability | Notes |
|--------|---------|------------------|-------|
| **form.load.getdoctype** | `{meta, docs[], docinfo, status, message}` (full form load) | **META** | Core frappe form loader; includes DocType meta + doc + docinfo |
| **form.assign_to.add** | Assignment doc | **CRUD** | Create ToDo assignment |
| **form.assign_to.add_multiple** | `null` | **CRUD** | Batch assign users |
| **form.assign_to.remove_multiple** | `null` | **CRUD** | Batch remove assignments |
| **doctype.event.event.update_attending_status** | `null` | **CRUD** | Update Event attendance status |
| **like.toggle_like** | `null` | **CRUD** | Toggle _liked_by field |
| **search.search_link** | `[{name, value}]` (filtered Link options) | **CRUD** | Autocomplete for Link fields |
| **doctype.bulk_update.bulk_update.submit_cancel_or_update_docs** | `null` | **CRUD** | Bulk submit/cancel/update |

---

## Response Shapes (8 Core Methods)

### 1. `crm.api.doc.get_data`

```json
{
  "data": [
    { "name": "LEAD-2024-001", "modified": "2024-06-10 10:30:00", ... }
  ],
  "columns": [
    { "label": "Name", "type": "Data", "key": "name", "width": "16rem" }
  ],
  "rows": ["name", "modified", ...],
  "fields": [
    { "label": "Lead Name", "fieldtype": "Data", "fieldname": "lead_name", "options": null }
  ],
  "column_field": null,
  "title_field": "name",
  "kanban_columns": null,
  "kanban_fields": null,
  "group_by_field": null,
  "page_length": 20,
  "page_length_count": 20,
  "is_default": true,
  "views": [...],
  "total_count": 150,
  "row_count": 20,
  "form_script": "...",
  "list_script": "...",
  "view_type": "list"
}
```

### 2. `crm.api.doc.get_filterable_fields`

```json
[
  {
    "fieldname": "status",
    "fieldtype": "Link",
    "label": "Status",
    "name": "status",
    "value": "status",
    "options": "CRM Lead Status"
  },
  {
    "fieldname": "_assign",
    "fieldtype": "Text",
    "label": "Assigned To",
    "name": "_assign",
    "value": "_assign",
    "options": null
  }
]
```

### 3. `crm.api.doc.get_quick_filters`

```json
[
  {
    "label": "Status",
    "fieldname": "status",
    "fieldtype": "Link",
    "options": [
      { "label": "New", "value": "New" },
      { "label": "Interested", "value": "Interested" }
    ]
  },
  {
    "label": "Source",
    "fieldname": "source",
    "fieldtype": "Link",
    "options": [
      { "label": "Website", "value": "Website" }
    ]
  }
]
```

### 4. `crm.api.doc.sort_options`

```json
[
  {
    "label": "Name",
    "fieldname": "name",
    "value": "name"
  },
  {
    "label": "Lead Name",
    "fieldname": "lead_name",
    "value": "lead_name"
  },
  {
    "label": "Created On",
    "fieldname": "creation",
    "value": "creation"
  }
]
```

### 5. `crm.api.views.get_views`

```json
[
  {
    "name": "view-1",
    "dt": "CRM Lead",
    "type": "list",
    "is_standard": 0,
    "user": "user@example.com",
    "columns": "[{\"key\":\"name\",...}]",
    "rows": "[\"name\",...]",
    "load_default_columns": 0
  }
]
```

### 6. `crm.api.session.get_users`

```json
[
  {
    "name": "user@example.com",
    "email": "user@example.com",
    "enabled": 1,
    "user_image": "https://...",
    "first_name": "John",
    "last_name": "Doe",
    "full_name": "John Doe",
    "user_type": "Website User",
    "language": "en",
    "roles": ["Sales User"],
    "role": "Sales User",
    "session_user": false,
    "is_telephony_agent": false
  }
]
```

### 7. `crm.api.session.get_organizations`

```json
[
  {
    "name": "ORG-001",
    "organization_name": "Acme Corp",
    "website": "https://acme.com",
    "industry": "Technology",
    "territory": "North America",
    "annual_revenue": 5000000.0,
    "creation": "2024-01-15 10:30:00",
    "modified": "2024-06-10 15:45:00"
  }
]
```

### 8. `crm.api.notifications.get_notifications`

```json
[
  {
    "creation": "2024-06-10 10:30:00",
    "from_user": {
      "name": "manager@example.com",
      "full_name": "Manager Name"
    },
    "type": "Assignment",
    "to_user": "user@example.com",
    "read": false,
    "hash": "#leads",
    "notification_text": "<div>...</div>",
    "notification_type_doctype": "CRM Lead",
    "notification_type_doc": "LEAD-2024-001",
    "reference_doctype": "lead",
    "reference_name": "LEAD-2024-001",
    "route_name": "Lead"
  }
]
```

---

## Method Classification by UI Feature Gate

### App Shell / Login Gate
- `frappe.client.get_doc` (load User doc)
- `crm.api.session.get_users`
- `crm.api.session.get_organizations`

### List View (Leads/Deals/Contacts/etc.)
- `crm.api.doc.get_data` ← **PRIMARY**
- `crm.api.doc.get_filterable_fields`
- `crm.api.doc.get_quick_filters`
- `crm.api.doc.sort_options`
- `crm.api.doc.get_group_by_fields`
- `crm.api.views.get_views`
- `frappe.client.get_list` (underlying)
- `crm.api.doc.get_fields` (optional, for column config)
- `frappe.desk.search.search_link` (for filter autocomplete)

### Single Record Detail View
- `frappe.desk.form.load.getdoctype` ← **PRIMARY**
- `crm.api.doc.get_assigned_users`
- `crm.api.doc.get_linked_docs_of_document`
- `crm.api.activities.get_activities` (Activity tab)
- `crm.api.notifications.get_notifications` (if shown)
- `frappe.desk.like.toggle_like`
- `frappe.desk.form.assign_to.add` / `add_multiple`
- `crm.api.whatsapp.get_whatsapp_messages` (if WhatsApp enabled)
- `frappe.client.set_value` (save field changes)

### Dashboard
- `crm.api.dashboard.get_dashboard` ← **PRIMARY**
- `crm.api.dashboard.get_chart` (per-chart refresh)
- All `dashboard.get_*` helper methods

### Sidebar / Quick Actions
- `crm.api.contact.get_linked_deals`
- `crm.api.comment.add_comment`
- `crm.api.doc.remove_assignments`
- `frappe.client.delete`
- `crm.api.notifications.mark_as_read`

---

## CONTROLLER-dependent Methods (Requires Native Implementation)

These methods have business logic beyond simple CRUD that depends on controller methods:

1. **`crm.api.doc.get_data`** → `CRMLead.default_list_data()`, `CRMLead.parse_list_data()`, `CRMLead.default_kanban_settings()`, `CRMDeal.default_list_data()`, etc.
   - **Fallback:** Use hardcoded defaults (columns, rows, kanban_fields); skip parse_list_data transform
   - **Risk:** Custom column layouts per role won't render; Kanban config missing

2. **`crm.api.doc.get_filterable_fields`** → `get_controller(doctype).get_non_filterable_fields()` hook
   - **Fallback:** Assume no restricted fields (all allowed fieldtypes are filterable)
   - **Risk:** Rare; custom restricted_fields not honored

3. **`crm.api.session.get_users`** → `CRM_ALLOWED_ROLES` check + role assignment logic
   - **Fallback:** Return all enabled users with roles; skip telephony_agent flag
   - **Risk:** Telephony integration broken

4. **`crm.api.activities.get_activities`** → `frappe.desk.form.load.get_docinfo()` (reads versions, comments, communications, attachments); document-specific history rendering
   - **Fallback:** Return empty or minimal activity (no version history)
   - **Risk:** Activity feed entirely missing or broken

5. **`crm.api.doc.get_linked_docs_of_document`** → `frappe.model.delete_doc.get_linked_docs()`, `get_dynamic_linked_docs()`; CRM-specific naming logic (show organization, lead_name, etc.)
   - **Fallback:** Query Dynamic Link table directly; skip CRM special cases
   - **Risk:** Linked doc titles may show docname instead of readable name

6. **`crm.api.dashboard.*`** → 20+ chart calculation methods, frappe Query Builder aggregations
   - **Fallback:** Return zeros or empty arrays per chart
   - **Risk:** Dashboard entirely non-functional

7. **`crm.api.whatsapp.get_whatsapp_messages`** → Template parameter parsing, reply/reaction chain resolution
   - **Fallback:** Return raw messages without enrichment
   - **Risk:** Template variables not interpolated; replies not threaded

8. **`crm.api.user.update_user_role`** → Role hierarchy enforcement, Sales Hierarchy cleanup
   - **Fallback:** Append roles directly; skip hierarchy checks
   - **Risk:** Role conflicts; orphaned hierarchy nodes

9. **`crm.api.exchange_rate.get_exchange_rate`** → Multi-provider API fallback logic + caching
   - **Fallback:** Single provider only; no fallback
   - **Risk:** Rate fetch fails if primary provider down

10. **`crm.api.doc.remove_linked_doc_reference`** → Doctype-specific unlinking logic (Contact vs Notification)
    - **Fallback:** Generic reference field clearing
    - **Risk:** Contact relationships not fully cleared

---

## Method Call Patterns in Frontend

### Primary Data Load Sequence
1. **Shell bootstrap:** `frappe.client.get_doc("User", session.user)` → `crm.api.session.get_users()` → `crm.api.session.get_organizations()`
2. **List view:** `crm.api.doc.get_data(doctype, filters, order_by, ...)` (single mega-call)
3. **Detail view:** `frappe.desk.form.load.getdoctype(doctype, name)` → `crm.api.doc.get_assigned_users()`, `get_linked_docs_of_document()` (parallel)
4. **Activity tab:** `crm.api.activities.get_activities(name)`

### Filtering & Sorting
- Build filters locally (JavaScript side); pass to `get_data(filters=...)`
- On field autocomplete: `frappe.desk.search.search_link(doctype, txt)`

### Notifications
- Poll `crm.api.notifications.get_notifications()` on page load + realtime socket
- Mark read: `crm.api.notifications.mark_as_read(user=..., doc=...)`

### Edit & Save
- For simple fields: `frappe.client.set_value(doctype, name, fieldname, value)`
- For complex (multi-field, child table): POST `/api/resource/{doctype}/{name}` (standard Frappe form save)
- Assignments: `frappe.desk.form.assign_to.add(doctype, name, user)`

---

## Rendering Dependency Matrix

| UI Feature | **TRIVIAL** | **META** | **CRUD** | **CONTROLLER** |
|-----------|-----------|---------|---------|----------------|
| Login gate | — | — | `get_doc`, `get_users` | — |
| List view header | — | `get_filterable_fields`, `sort_options` | — | — |
| List rows (data) | — | — | — | **`get_data`** |
| Quick filters | — | **`get_quick_filters`** | — | — |
| Views selector | — | — | **`get_views`** | — |
| Detail form | — | **`getdoctype` (meta)** | — | — |
| Activity feed | — | — | — | **`get_activities`** |
| Linked docs | — | — | — | **`get_linked_docs_of_document`** |
| Assignments | — | — | **`get_assigned_users`** | — |
| WhatsApp tab | **`is_whatsapp_enabled`** | — | — | **`get_whatsapp_messages`** |
| Dashboard | — | — | — | **`get_dashboard`, `get_chart`, + 20 helpers** |
| Notifications | — | — | — | **`get_notifications`** |

