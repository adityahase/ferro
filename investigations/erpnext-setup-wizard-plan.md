# ERPNext Setup Wizard: Implementation Plan for Pure-Rust ferro Runtime

## Overview

This plan details making the Frappe Desk setup wizard RENDER and COMPLETE on a fresh ERPNext site served by the pure-Rust ferro runtime (which runs app Python via embedded CPython shim "ferrod").

Currently, ferro HARDCODES `setup_complete=1` in boot (desk.rs:189–191), forcing the workspace view. The wizard never appears or runs, preventing tenant provisioning workflows.

---

## 1. TRIGGER: Boot Detection of Setup Completion

### 1.1 Current State (ferro desk.rs:189–191)

```rust
if let Some(Value::Object(ref mut sd)) = o.get_mut("sysdefaults") {
    sd.insert("setup_complete".into(), json!("1"));
}
o.insert("setup_complete".into(), json!(1));
```

**Problem**: Always true, wizard never appears.

### 1.2 Trigger Logic (Frappe Framework)

The Desk frontend (`frappe/desk/page/setup_wizard/setup_wizard.js:34`) checks:

```javascript
if (frappe.boot.setup_complete) {
    window.location.href = frappe.boot.apps_data.default_path || "/desk";
}
```

And the backend (frappe/boot.py) conditionally adds setup data:

```python
if not frappe.is_setup_complete():
    bootinfo.setup_wizard_requires = frappe.get_hooks("setup_wizard_requires")
```

### 1.3 Completion Detection Logic

**Frappe** (`frappe/__init__.py::is_setup_complete()`):
- Checks `Installed Application` table for `is_setup_complete` flags for both `frappe` AND `erpnext`.
- Returns `True` only if ALL critical apps have `is_setup_complete=1`.

**ERPNext** (`erpnext/core/doctype/installed_applications/installed_applications.py:40–45`):
- Sets `is_setup_complete=1` for frappe if: any non-admin user exists (`has_non_admin_user()`)
- Sets `is_setup_complete=1` for erpnext if: any Company exists (`has_company()`)

### 1.4 Recommended ferro Implementation

In `src/desk.rs`, replace the hardcoded true with dynamic detection:

**Logic** (pseudocode):
```
fn build_boot(...) -> bool {
    // Detect setup completion:
    // 1. Check if `Installed Application` table exists
    // 2. If not → setup_complete = false (early site, show wizard)
    // 3. If yes:
    //    - Query: SELECT COUNT(*) WHERE app_name IN ('frappe','erpnext') AND is_setup_complete=1
    //    - If COUNT == 2 → setup_complete = true (both done, hide wizard)
    //    - Else → setup_complete = false (one or both incomplete, show wizard)
    //
    // Alternative (simpler, used by ERPNext logic):
    //    - Check if Company table exists AND has ≥1 row → setup_complete = true for erpnext
    //    - Otherwise → false
}
```

**Simplest Path** (recommended for MVP):
- Query `SELECT COUNT(*) FROM Company LIMIT 1` (SQLite native)
- If result is 0 → wizard needed → `setup_complete = false`
- If result is 1+ → wizard done → `setup_complete = true`

This mirrors ERPNext's `has_company()` check (installed_applications.py:103–107).

---

## 2. RENDER: Frontend Methods Called & Hook System

### 2.1 Frontend Method Calls (in order)

When wizard page loads, the frontend calls (from setup_wizard.js):

**On Page Load** (lines 39–69):
1. **`frappe.desk.page.setup_wizard.setup_wizard.load_languages`** → returns:
   ```json
   {
     "default_language": "English",
     "languages": [{"value": "English", "label": "English", "description": "en"}, ...],
     "codes_to_names": {"en": "English", ...}
   }
   ```

2. **`frappe.desk.page.setup_wizard.setup_wizard.load_user_details`** → returns:
   ```json
   {
     "full_name": "...",  // from frappe.cache.hget("email", "signup")
     "email": "..."
   }
   ```

**During Wizard Interaction** (ERPNext-specific):
3. **`erpnext.accounts.doctype.account.chart_of_accounts.chart_of_accounts.get_charts_for_country`**
   - Takes: `country` (e.g., "United States")
   - Returns: list of Chart of Accounts templates (e.g., `["Standard", "Standard with Categories", ...]`)

**On Completion** (line 214):
4. **`frappe.desk.page.setup_wizard.setup_wizard.setup_complete`**
   - Takes: `args` dict with full wizard state (see §2.3 below)
   - Returns: `{"status": "ok"}` or error

### 2.2 Hook System: setup_wizard_requires & setup_wizard_stages

**From hooks.py** (both Frappe and app-specific):

**Frappe built-in** (frappe/desk/page/setup_wizard/setup_wizard.js:86–92):
- Ships hard-coded `frappe.setup.slides_settings` with "welcome" + "user" slides
- No hook needed, baked into JS

**ERPNext** (erpnext/hooks.py:63–64):
```python
setup_wizard_requires = "assets/erpnext/js/setup_wizard.js"
setup_wizard_stages = "erpnext.setup.setup_wizard.setup_wizard.get_setup_stages"
```

- `setup_wizard_requires`: **JS files to load** before rendering (contains slide definitions)
  - Frappe loads this as: `frappe.require(requires, callback)` (setup_wizard.js:38–39)
  - ferro must inject these into boot as `setup_wizard_requires: ["assets/erpnext/js/setup_wizard.js", ...]`

- `setup_wizard_stages`: **Callable method** during `setup_complete` to fetch stages
  - Called by `get_setup_stages()` (setup_wizard.py:20–49)
  - ERPNext defines via `erpnext.setup.setup_wizard.setup_wizard.get_setup_stages(args)` → returns list of stages with tasks

### 2.3 setup_complete Args Shape

From the wizard UI (setup_wizard.js + erpnext/setup_wizard.js), the args dict contains:

**Frappe slides** (welcome + user):
- `language`: "English"
- `country`: "United States"
- `timezone`: "America/New_York"
- `currency`: "USD"
- `enable_telemetry`: 0 or 1
- `full_name`: "John Doe"
- `email`: "admin@example.com"
- `password`: "***" (if Administrator, else absent)

**ERPNext organization slide**:
- `company_name`: "ACME Inc"
- `company_abbr`: "ACME"
- `chart_of_accounts`: "Standard" (or other template)
- `fy_start_date`: "2024-01-01"
- `fy_end_date`: "2025-12-31"
- `setup_demo`: 0 or 1

### 2.4 Setup Wizard Rendering Dependency: Which Methods Must Be Native in ferro?

**MUST be native (cannot be routed to Python)**:
- **`load_languages`**: returns hardcoded Language doctype query; uses simple frappe.db.
  - **Shim gap**: Needs `frappe.qb.from_().select().run(as_dict=1)` support
  - **Effort**: LOW (shim already has pypika/qb)

- **`load_user_details`**: calls `frappe.cache.hget("email", "signup")`
  - **Shim gap**: Needs frappe.cache mock (redis-like hget)
  - **Effort**: MEDIUM (cache abstraction)

- **`get_charts_for_country`** (ERPNext): queries Account doctype filtered by country
  - **Shim gap**: Needs generic doctype `.get_all()` query
  - **Effort**: LOW (shim has get_all)

**Can route to Python** (via ferrod shim if available):
- **`setup_complete`**: The heavy lifter; delegates to setup stages

---

## 3. COMPLETE: setup_complete Workflow & Shim Gaps

### 3.1 setup_complete Call Chain

From setup_wizard.py:52–69:

```python
@frappe.whitelist()
def setup_complete(args):
    with filelock("setup_wizard", timeout=0.5):
        if frappe.is_setup_complete():
            return {"status": "ok"}
        
        kwargs = parse_args(sanitize_input(args))
        stages = get_setup_stages(kwargs)
        return process_setup_stages(stages, kwargs)
```

Calls `get_setup_stages(kwargs)` which:
1. Adds initial "Updating global settings" stage (calls `update_global_settings`)
2. Calls `get_stages_hooks(args)` → **for each installed app, calls its `setup_wizard_stages` hook** (e.g., erpnext.setup.setup_wizard.setup_wizard.get_setup_stages)
3. Calls `get_setup_complete_hooks(args)` → **calls methods from `setup_wizard_complete` hooks**
4. Adds final "Wrapping up" stage (calls `run_post_setup_complete`)

Then calls `process_setup_stages(stages, kwargs)` which:
- Iterates stages, publishes realtime progress (`setup_task`)
- Calls each task's `fn(args)`
- Tracks completion per-app in `Installed Application.is_setup_complete`

### 3.2 ERPNext Setup Stages (from erpnext/setup/setup_wizard/setup_wizard.py:12–42)

**Stage 1: Installing presets** → `stage_fixtures(args)` → `fixtures.install(args.country)`
**Stage 2: Setting up company** → `setup_company(args)` → `fixtures.install_company(args)`
**Stage 3: Setting defaults** → `setup_defaults(args)` → `fixtures.install_defaults(args)`

Each relies on `frappe.new_doc().insert()` and DDL-free operations.

### 3.3 Stage 1: Fixtures (erpnext/setup/setup_wizard/operations/install_fixtures.py)

**Functions called** (key gaps listed):

| Function | Key Operations | Shim Gap? |
|---|---|---|
| `install(country)` | Creates ~90 records (Item Group, Territory, Customer Group, etc.) via `make_records(records)` → loops `frappe.new_doc().insert()` | **LOW** (already have doc creation) |
| `install()` also calls | Reads JSON files `uom_data.json`, `uom_conversion_data.json` from disk → creates UOM/UOM Conversion Factor records | **HIGH** (file I/O + from_dict creation path) |
| `install()` also calls | `update_selling_defaults()` → `frappe.get_doc("Selling Settings").save()` | **MEDIUM** (get_single, save) |
| `install()` also calls | `update_buying_defaults()` → similar | **MEDIUM** |
| `install()` also calls | `add_uom_data()` → opens file handle; `frappe.db.insert()` (direct SQL) | **HIGH** |
| `install()` also calls | `update_item_variant_settings()` → `frappe.get_doc("Item Variant Settings"); doc.set_default_fields()` | **MEDIUM** (method call) |
| `install()` also calls | `set_up_address_templates()` | **MEDIUM** |
| `install()` also calls | `update_global_search_doctypes()` | **MEDIUM** |

### 3.4 Stage 2: Company Setup (erpnext/setup/setup_wizard/operations/install_fixtures.py:454–477)

```python
def install_company(args):
    records = [
        {"doctype": "Fiscal Year", "year": ..., "year_start_date": ..., "year_end_date": ...},
        {"doctype": "Company", "company_name": ..., "abbr": ..., "default_currency": ..., "country": ..., 
         "create_chart_of_accounts_based_on": "Standard Template",
         "chart_of_accounts": args.chart_of_accounts}
    ]
    make_records(records)
```

**Key gap**: Creating Company with `chart_of_accounts` template triggers a **hook** in Company.on_update that generates CoA accounts by reading a JSON template file from disk.

**Shim gap**: **CRITICAL** — Must support `frappe.get_doc(...).insert()` with child-table inserts AND hooks/on_update callbacks.

### 3.5 Stage 3: Defaults Setup (erpnext/setup/setup_wizard/operations/install_fixtures.py:480–545)

```python
def install_defaults(args):
    records = [
        {"doctype": "Price List", "price_list_name": "Standard Buying", ...},
        {"doctype": "Price List", "price_list_name": "Standard Selling", ...}
    ]
    make_records(records)
    frappe.db.set_value("Currency", args.currency, "enabled", 1)
    frappe.db.set_single_value("Stock Settings", "email_footer_address", args.company_name)
    set_global_defaults(args)
    update_stock_settings()
    create_bank_account(args)
```

**Key operations** (shim gaps):

| Operation | Shim Gap |
|---|---|
| `frappe.db.set_value()` (simple single-table update) | **LOW** |
| `frappe.db.set_single_value()` (single-table update for "singles" like Stock Settings) | **LOW** |
| `set_global_defaults(args)` → `frappe.get_doc("Global Defaults", "Global Defaults").update().save()` | **MEDIUM** (singles doctype) |
| `update_stock_settings()` → similar | **MEDIUM** |
| `create_bank_account(args)` → creates Account doctype (parent-child: tree structure) | **MEDIUM** (tree nesting) |

### 3.6 Critical Shim Gaps Summary

| Gap | Severity | Impact | Fix Priority |
|---|---|---|---|
| **File I/O** (reading `uom_data.json`, chart templates, fixtures) | HIGH | Can't load fixtures without file reads | P0 |
| **Company.on_update hook** (generates CoA from template) | CRITICAL | Can't create valid Company → CoA uninitialized | P0 |
| **frappe.get_doc().insert()** with child tables | MEDIUM | Price Lists, Bank Accounts, etc. need child rows | P1 |
| **frappe.get_doc("DocType").save()** for singles | MEDIUM | System Settings, Stock Settings, Selling Settings | P1 |
| **frappe.cache.hget()** | LOW | Only for `load_user_details` (preload cache or skip) | P2 |
| **frappe.local.flags.in_setup_wizard** | MEDIUM | Flag to enable special perms during setup | P1 |
| **realtime publish** (`frappe.publish_realtime("setup_task", ...)`) | LOW | Progress bars; can be stubbed or no-op | P2 |

---

## 4. PHASED IMPLEMENTATION PLAN

### Phase A: Render the Wizard (enable boot detection + frontend asset loading)

**Effort**: ~3 days  
**Scope**: Make the wizard UI appear; no operations yet (no Company creation).

#### A.1 Update ferro desk.rs: Dynamic setup_complete Detection
- **File**: `/home/frappe/ferro/src/desk.rs`, `build_boot()` method (around line 180–228)
- **Change**:
  ```rust
  // Replace lines 189–191:
  // OLD:
  // sd.insert("setup_complete".into(), json!("1"));
  // o.insert("setup_complete".into(), json!(1));
  
  // NEW:
  let setup_complete = self.detect_setup_complete(con);
  
  if let Some(Value::Object(ref mut sd)) = o.get_mut("sysdefaults") {
      sd.insert("setup_complete".into(), json!(setup_complete as i32));
  }
  o.insert("setup_complete".into(), json!(setup_complete as i32));
  ```

- **New method** in Desk struct:
  ```rust
  fn detect_setup_complete(&self, con: &Connection) -> bool {
      // Query Company table
      match con.query_row(
          "SELECT COUNT(*) FROM Company LIMIT 1",
          [],
          |row| row.get::<_, i64>(0),
      ) {
          Ok(count) => count > 0,
          Err(_) => false, // table doesn't exist or query fails → assume incomplete
      }
  }
  ```

#### A.2 Update desk.rs: Populate setup_wizard_requires in boot
- **File**: `/home/frappe/ferro/src/desk.rs`, `build_boot()` method
- **Change**: After setting up boot object, add:
  ```rust
  // Inject setup_wizard_requires for non-complete setups
  if !setup_complete {
      let requires = self.get_setup_wizard_requires(con);
      o.insert("setup_wizard_requires".into(), json!(requires));
  }
  ```

- **New method**:
  ```rust
  fn get_setup_wizard_requires(&self, con: &Connection) -> Vec<String> {
      // Hard-code for now (or query hook registry if available)
      // FROM erpnext/hooks.py:63:
      vec![
          "assets/erpnext/js/setup_wizard.js".to_string(),
      ]
  }
  ```
  (Later: integrate with hook registry from metadata)

#### A.3 Verify Desk frontend can load the wizard
- **Test**: Start fresh ferro site (no Company) → navigate to `/desk` → should land on `/app/setup-wizard/0` instead of workspace
- **Blockers**: None expected; frontend JS is unchanged

---

### Phase B: Complete the Wizard (implement stages, ferrod integration)

**Effort**: ~2 weeks (depending on shim maturity)  
**Scope**: Execute all setup stages to create Company, CoA, Defaults.

#### B.1 Implement ferro route for setup_complete (main.rs)

**File**: `/home/frappe/ferro/src/main.rs`

Add handler before desk handler (around line 990+):
```rust
// /api/method/frappe.desk.page.setup_wizard.setup_wizard.setup_complete
if path == "/api/method/frappe.desk.page.setup_wizard.setup_wizard.setup_complete" 
    && method == "POST"
    && app.pyfall.is_some() {
    // Route to ferrod for full setup execution
    return call_python_method(app.pyfall.as_ref().unwrap(), path, req, site).await;
}
```

#### B.2 Implement ferro routes for render methods (main.rs)

Routes MUST be native Rust (not Python):
- `/api/method/frappe.desk.page.setup_wizard.setup_wizard.load_languages`
- `/api/method/frappe.desk.page.setup_wizard.setup_wizard.load_user_details`
- `/api/method/erpnext.accounts.doctype.account.chart_of_accounts.chart_of_accounts.get_charts_for_country`

**File**: `/home/frappe/ferro/src/main.rs`

```rust
match path {
    "/api/method/frappe.desk.page.setup_wizard.setup_wizard.load_languages" => {
        return load_languages(con, req).await;
    }
    "/api/method/frappe.desk.page.setup_wizard.setup_wizard.load_user_details" => {
        return load_user_details(con).await;
    }
    "/api/method/erpnext.accounts.doctype.account.chart_of_accounts.chart_of_accounts.get_charts_for_country" => {
        return get_charts_for_country(con, req).await;
    }
    _ => {}
}
```

#### B.3 Implement load_languages (new in desk.rs or separate handler)

```rust
async fn load_languages(con: &Connection) -> HttpResponse {
    // Query Language doctype
    let languages = con.prepare("
        SELECT language_name, language_code FROM `tabLanguage` WHERE enabled = 1 ORDER BY language_code
    ")
    .and_then(|mut stmt| {
        stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .and_then(|rows| {
            let mut langs = Vec::new();
            for r in rows {
                if let Ok((name, code)) = r {
                    langs.push(json!({
                        "label": name,
                        "value": name,
                        "description": code
                    }));
                }
            }
            Ok(langs)
        })
    })
    .unwrap_or_default();
    
    let resp = json!({
        "default_language": "English",
        "languages": languages,
        "codes_to_names": {} // populate from Language table
    });
    
    HttpResponse::Ok().json(resp)
}
```

#### B.4 Implement load_user_details

```rust
async fn load_user_details() -> HttpResponse {
    // For now, return empty (cache not available in pure-Rust)
    // Later: query User table for first non-admin user
    let resp = json!({
        "full_name": "",
        "email": ""
    });
    HttpResponse::Ok().json(resp)
}
```

#### B.5 Implement get_charts_for_country

```rust
async fn get_charts_for_country(con: &Connection, req: HttpRequest) -> HttpResponse {
    let country = get_query_param(&req, "country");
    
    // Fetch from Chart of Accounts master (DocType)
    // For MVP: hardcode standard templates
    let charts = vec![
        "Standard",
        "Standard with Categories",
        // ... per country
    ];
    
    HttpResponse::Ok().json(charts)
}
```

#### B.6 Enable ferrod (embed CPython) for setup_complete

**Critical**: setup_complete MUST run Python because:
- Company.insert() triggers hook that reads chart-of-accounts JSON from disk
- Full stage pipeline calls Python methods (erpnext.setup.setup_wizard.setup_wizard.get_setup_stages)

**Setup**:
1. Ensure ferro binary is built WITH ferrod support: `cargo build --features ferrod`
2. Update deployment to use ferrod-enabled binary (e.g., `ferro-py` binary)
3. Server.py (Frappe's actual server) must set `web_runtime=ferrod` in site config
4. Ensure pyenv/Python 3.13 is available on the deployment box

**In main.rs**: Ensure `app.pyfall` is initialized when ferrod feature is enabled.

---

### Phase C: Simplify for MVP (Optional: Programmatic Setup)

**Effort**: ~2 days  
**Scope**: Provide a non-interactive setup option (programmatic API) as a provisioning shortcut.

#### C.1 New Endpoint: /api/method/erpnext.setup.setup_wizard.setup_wizard.setup_complete

**Observation**: ERPNext's setup_wizard.py already exports a programmatic `setup_complete(args)` function (line 62–65):

```python
def setup_complete(args=None):
    stage_fixtures(args)
    setup_company(args)
    setup_defaults(args)
```

This can be called directly WITHOUT the interactive wizard, suitable for provisioning automation.

#### C.2 Route in ferro (main.rs)

```rust
if path == "/api/method/erpnext.setup.setup_wizard.setup_wizard.setup_complete" && app.pyfall.is_some() {
    // Programmatic setup (skip wizard UI)
    return call_python_method(app.pyfall.as_ref().unwrap(), path, req, site).await;
}
```

#### C.3 Usage (provisioning script)

```bash
# Instead of rendering wizard UI:
curl -X POST "https://tenant.ferro.dev/api/method/erpnext.setup.setup_wizard.setup_wizard.setup_complete" \
  --header "Content-Type: application/json" \
  --data '{
    "company_name": "ACME Inc",
    "company_abbr": "ACME",
    "country": "United States",
    "currency": "USD",
    "chart_of_accounts": "Standard",
    "fy_start_date": "2024-01-01",
    "fy_end_date": "2025-12-31"
  }'
```

**Benefit**: Tenants can be set up without interactive browser; suitable for `signup.html` → auto-fill form → one-click create.

---

## 5. RECOMMENDATION: MVP Path (Fastest to Feature-Complete)

### Recommended Approach (8–10 days total)

1. **Phase A (Days 1–3)**: Render the wizard
   - Update desk.rs setup_complete detection (~4 hours)
   - Add setup_wizard_requires to boot (~2 hours)
   - Verify frontend loads wizard UI (~1 hour)
   - **No ferrod needed yet**

2. **Phase B.1–B.5 (Days 4–6)**: Native render methods
   - Implement load_languages, load_user_details, get_charts_for_country in pure Rust (~1 day)
   - Routes in main.rs to serve them (~4 hours)
   - Test wizard form displays correctly (~4 hours)
   - **Still no ferrod**

3. **Phase B.6 + setup_complete (Days 7–10)**: Hook ferrod for completion
   - Build ferrod binary (or use existing `ferro-py`) (~1 hour)
   - Route /api/method/.../setup_complete to ferrod (~2 hours)
   - Test full wizard workflow end-to-end (~1 day)

4. **Phase C (Optional, Days 11–12)**: Programmatic setup
   - Add direct route to erpnext.setup_complete() (~2 hours)
   - Integration test provisioning script (~2 hours)

### Why This Order?

- **Phases A + B render** unblock the UX immediately (wizard appears) without Python
- **Phase B.6 setup_complete** is only needed when user clicks "Complete"
- **Phase C** is convenience for provisioning (not required for manual signup flow)

---

## 6. Appendix: Shim Checklist for ferrod

### Must Exist in shim/framework/frappe for setup stages to work:

| Module | Functions/Attributes | Status |
|---|---|---|
| `frappe.new_doc()` | Create new doc | ✓ (shim has) |
| `doc.insert()` | Save new doc | ✓ (shim has) |
| `doc.save()` | Update existing | ✓ (shim has) |
| `frappe.get_doc(doctype, name)` | Fetch single | ✓ (shim has) |
| `frappe.db.set_value()` | Simple SQL update | ✓ (shim has) |
| `frappe.db.set_single_value()` | Update single doctype | ✓ (shim has) |
| `frappe.get_all()` | Query multiple | ✓ (shim has) |
| `frappe.get_hooks()` | Get hook methods | **NEEDS** (for setup_wizard_stages/setup_wizard_complete) |
| `frappe.get_attr()` | Import & call method by dotted path | **NEEDS** |
| `frappe.flags.in_setup_wizard` | Context flag | **NEEDS** (to enable perms) |
| `frappe.local.form_dict` | Request body parsing | ✓ (shim has) |
| `frappe.db.savepoint()` / `rollback()` | Transaction control | ✓ (shim has) |
| `frappe.db.commit()` | Commit transaction | ✓ (shim has) |
| **File I/O** | `frappe.read_file()`, `open()` | **NEEDS** (for fixture JSON, chart templates) |
| **Realtime** | `frappe.publish_realtime()` | Can stub (no-op) |

### Known Gaps to Close Before setup_complete Works:

1. **Hook System**: Must be able to call `frappe.get_hooks("setup_wizard_stages")` and invoke the methods
2. **File I/O**: Must support reading JSON fixtures from disk (embedded in binary or via shim file system)
3. **Company.on_update Hooks**: Embedded Frappe code must run on Company creation (triggers CoA generation)

---

## 7. Files to Modify / Create

### Modify:

1. **`/home/frappe/ferro/src/desk.rs`**
   - `detect_setup_complete()` method
   - `get_setup_wizard_requires()` method
   - Update `build_boot()` to use dynamic setup_complete

2. **`/home/frappe/ferro/src/main.rs`**
   - Add routes for load_languages, load_user_details, get_charts_for_country
   - Route setup_complete to ferrod

### Create:

3. **`/home/frappe/ferro/src/setup_wizard.rs`** (or fold into desk.rs)
   - `load_languages()` handler
   - `load_user_details()` handler
   - `get_charts_for_country()` handler
   - Helper: query Language, User, Chart of Accounts tables

### Leverage (unchanged):

4. **Frappe framework code** (already in shim):
   - `frappe/desk/page/setup_wizard/setup_wizard.py` (routes to ferrod)
   - `frappe/desk/page/setup_wizard/setup_wizard.js` (frontend unchanged)
   - `erpnext/setup/setup_wizard/setup_wizard.py` (runs in ferrod)
   - `erpnext/setup/setup_wizard/operations/install_fixtures.py` (runs in ferrod)

---

## 8. Success Criteria

- [ ] **Render**: Fresh ERPNext site (no Company) served by ferro → navigate to /desk → Desk redirects to /app/setup-wizard/0, wizard appears
- [ ] **Load Languages**: Wizard's "Welcome" slide populates language, country, timezone, currency dropdowns correctly
- [ ] **Load Company Form**: Wizard's "Organization" slide (ERPNext-specific) appears with company name, CoA dropdown, fiscal year fields
- [ ] **Chart of Accounts Dropdown**: Selecting a country populates chart_of_accounts options (Standard, Standard with Categories, etc.)
- [ ] **Complete Wizard**: Clicking "Complete" calls setup_complete(args), executes all stages, creates Company + Fiscal Year + Chart of Accounts + Defaults
- [ ] **Verify Completion**: After wizard completes, site is fully functional (workspace loads, lists render, forms work), setup_complete=true in subsequent boots

---

## 9. Risk Mitigations

| Risk | Mitigation |
|---|---|
| **ferrod overhead** (Phases B.6+) | Test with minimal Python (just setup hooks); consider lazy-loading PyO3 |
| **File I/O in shim** (chart JSON) | Pre-bake fixtures into binary or mount read-only /assets in container |
| **Hook system not ready** | Hardcode get_setup_stages() return in Rust until hook registry is available |
| **Company.on_update hook** (CoA generation) | Must have working Company + Account Rust models with hook invocation |
| **Realtime progress** (setup_task events) | Initially stub `frappe.publish_realtime()` as no-op; add later if needed |

---

## Conclusion

The setup wizard is RENDERABLE by ferro without major changes:
- **Phase A** (boot detection + requires injection): Pure Rust, ~3 days, unblocks UX
- **Phase B** (render methods + ferrod setup): Rust + Python, ~7 days, enables full setup
- **Phase C** (programmatic API): Bonus convenience feature, ~2 days

**Critical path**: ferrod must have working Document.insert() + hook system to handle Company creation and CoA template expansion. Coordinate with ferrod development in parallel.

**MVP shipping window**: 2 weeks.
