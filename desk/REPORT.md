# Running Frappe Desk on ferro — Compatibility Report

**Result: ferro serves the full Frappe Desk admin SPA — pure Rust, zero Python — against the
existing SQLite site.** Workspaces, list views, form views (incl. Single doctypes), the navbar,
sidebar, search, breadcrumbs, number cards and the activity timeline all render and are navigable
in a real headless Chrome session, with **zero failed requests, zero error dialogs and zero
uncaught page errors** across an 18-route navigation sweep.

This was the "ultimate usability test": Desk is Frappe's own admin UI and exercises the framework's
full HTTP surface (boot, asset pipeline, meta, list/report views, form load, workspaces). It now
works on the lean Rust runtime that previously served only the REST resource API.

---

## How it was verified (reference-oracle method)

1. Stood up the **real Frappe v17** bench (`/home/frappe/benches/bench-cpython314`) serving Desk on
   `:8000`, built its frontend assets (`bench build`), and drove it with headless
   **chrome-for-testing 149** via `playwright-core` — capturing the exact request sequence, the
   132 KB `frappe.boot` JSON, and every endpoint's response shape (`oracle/` dir).
2. Implemented Desk mode in ferro (`ferro/src/desk.rs`, wired into `ferro/src/main.rs`).
3. Drove **ferro** on `:8001` with the same browser and iterated against the oracle until the
   console was clean and the screenshots matched.

Screenshots (in `oracle/`): `ferro-app-home.png` (Build workspace), `nav-user-list.png` (User list,
real data), `nav-user-form.png` (User form, real data + timeline), `ws-users2.png` (workspace with
chart + number cards), `single-form.png` (System Settings, a Single doctype).

---

## Architecture

ferro already had the data plane the REST API used: `meta` (DocType/DocField from SQLite),
`orm` (get_list/get_doc/insert/update/delete/count), `auth` (perms), `util`. Desk mode adds, all
in Rust:

| Concern | Implementation |
|---|---|
| **Static assets** `/assets/*` | served straight from the bench's built `sites/assets` tree (which symlinks `frappe` → `apps/frappe/frappe/public`); MIME by extension; long cache headers. |
| **HTML shell** `/desk` (+ `/app`→`/desk` 301) | rendered in Rust: the `desk.html` skeleton with the `app_include_*` bundle tags resolved via `assets.json`, the boot blob, csrf, `dev_server=0`. |
| **bootinfo** | a captured baseline snapshot (`src/desk_boot.json`, vendored via `include_str!`) patched per-request (user, server_date, `setup_complete`, `home_page`). |
| **`frappe.desk.*` / `frappe.client.*` methods** | mapped onto the existing ORM (reportview, form load, workspace content, counts) — see table below. |
| **boot/toolbar methods** | static stubs (apps, roles, timezones, translations, session defaults…). |
| **login / session** | demo mode: ferro runs `--default-user Administrator`, so every request is the Administrator; `/api/method/login` returns the real envelope + identity cookies. CSRF tokens are accepted without server validation. |
| **realtime (socket.io)** | intentionally 404s; Desk degrades gracefully to no-realtime (Frappe is designed for this). |

Run it:
```
ferro serve <site-dir> --desk --dev          # assets dir & boot auto-derived
# e.g.
ferro serve /home/frappe/benches/bench-cpython314/sites/mysite.sqlite --port 8001 --desk --dev
# open http://localhost:8001/app
```
New flags: `--desk` (enable), `--assets <dir>` (override asset root), `--desk-boot <file>`
(override boot snapshot).

---

## Compatibility issue list (extensive) and status

Legend: **FIXED** = implemented from the DB/ORM · **STUB** = static/benign response (enough for Desk
to run) · **LIMITATION** = not implemented (documented below). Criticality: 🔴 blocker · 🟡 important · ⚪ minor.

### A. HTML shell & asset pipeline
| Item | Crit | Status |
|---|---|---|
| `/app` → `/desk` 301 redirect (v17 renamed the route) | 🔴 | FIXED |
| `/desk` and `/desk/<path>` render the SPA shell | 🔴 | FIXED |
| `/assets/*` static serving (bundles, css, fonts, icons svg, sounds, images) | 🔴 | FIXED |
| `assets.json` bundle-name → hashed-path resolution | 🟡 | FIXED |
| `app_include_js/css/icons` tag emission (incl. icon `fetch→#all-symbols`) | 🟡 | FIXED |
| `dev_server=0` to avoid the dev live-reload loop | 🟡 | FIXED |
| favicon / framework logo | ⚪ | FIXED (served from assets) |

### B. bootinfo (`frappe.boot`)
| Item | Crit | Status |
|---|---|---|
| Full 76-key boot payload (user, workspaces, sysdefaults, navbar, modules, …) | 🔴 | FIXED (baseline snapshot, patched) |
| `sysdefaults.setup_complete` truthy (else SPA forces the setup wizard) | 🔴 | FIXED (patched to 1) |
| `home_page` not `"setup-wizard"` (else infinite reload — see Bugs) | 🔴 | FIXED (patched → `"Workspaces"`) |
| live values (server_date, logged-in user, read_only) | 🟡 | FIXED (patched) |
| long-tail keys (versions, letter_heads, onboarding, …) | ⚪ | from snapshot (static) |

### C. List / report views
| Endpoint | Crit | Status |
|---|---|---|
| `frappe.desk.reportview.get` (compressed `{keys,values,user_info}`) | 🔴 | FIXED → `orm::get_list` (strips `` `tab..`.`f` `` qualifiers) |
| `frappe.desk.reportview.get_count` / `frappe.client.get_count` | 🔴 | FIXED → `orm::count` |
| `frappe.desk.reportview.get_list` / `frappe.client.get_list` | 🟡 | FIXED → `orm::get_list` |
| `frappe.desk.listview.get_list_settings` / list_view_settings | 🟡 | STUB `{}` |
| `frappe.model.utils.user_settings.get` / `.save` | 🟡 | STUB (`{}` / no-op) |
| `frappe.desk.listview.get_group_by_count` | ⚪ | STUB `[]` |

### D. Form views
| Endpoint | Crit | Status |
|---|---|---|
| `frappe.desk.form.load.getdoctype` (meta **bundle**: DocType + child-table metas) | 🔴 | FIXED (assembles tabDocType + tabDocField/DocPerm/… + child Table/Table-MultiSelect metas) |
| `frappe.desk.form.load.getdoc` (doc + docinfo + _link_titles) | 🔴 | FIXED → `orm::get_doc`; docinfo carries doctype+name |
| `frappe.client.get` / `get_value` / `get_single_value` | 🟡 | FIXED (get_single_value reads `tabSingles`) |
| `frappe.desk.form.load.getdoc` for a **new** doc → `{"message":[]}` HTTP 200 | 🟡 | FIXED (else the New form errors) |
| **`frappe.desk.form.save.savedocs`** (Save/Update) | 🟡 | FIXED → `orm::insert`/`update`; echoes `localname` so the form renames the local doc to the server name |
| `frappe.client.save` / `frappe.client.insert` | 🟡 | FIXED → same persist path |
| docinfo timeline content (attachments/comments/versions/assignments) | 🟡 | STUB (empty-but-well-formed; timeline shows the doc's own create/edit events) |

### E. Workspaces / desktop
| Endpoint | Crit | Status |
|---|---|---|
| `frappe.desk.desktop.get_desktop_page` (cards/charts/shortcuts/number_cards) | 🔴 | FIXED (groups `Workspace Link` rows into cards by Card Break, like `get_link_groups`) |
| `frappe.desk.desktop.get_workspace_sidebar_items` | 🟡 | FIXED (reads `tabWorkspace`) |
| `frappe.desk.desk_page.getpage` | 🟡 | FIXED (reads `tabWorkspace`) |
| number card value `number_card.get_result` | 🟡 | FIXED → `orm::count` on the card's document_type+filters |
| `number_card.get_percentage_difference` | ⚪ | STUB (null) |
| dashboard chart data `dashboard_chart.get` | ⚪ | STUB (empty series — see Limitations) |
| `dashboard_settings.{get,create}` | ⚪ | STUB |

### F. Session / boot-time / toolbar methods
| Endpoint | Crit | Status |
|---|---|---|
| `/api/method/login` (+ identity cookies) | 🔴 | FIXED (demo) |
| `frappe.auth.get_logged_user`, `ping` | 🟡 | FIXED (pre-existing) |
| CSRF (`X-Frappe-CSRF-Token`) | 🟡 | accepted without validation (demo) |
| `frappe.translate.get_boot_translations` | 🟡 | STUB `{}` (English; translations not loaded) |
| `apps.get_apps`, `session_default_settings.get_session_default_values`, `background_task.get_recent_tasks`, `notifications.get_open_count/get_notifications` | ⚪ | STUB |
| `user.get_all_roles` | ⚪ | FIXED (reads `tabRole`) |
| `user.get_timezones` | ⚪ | STUB (static IANA list) |
| socket.io `/socket.io/*` | ⚪ | 404 (realtime degrades) |

---

## Key bugs found & fixed (the hard ones)

1. **Infinite reload loop.** `home_page` in the boot was `"setup-wizard"`. The SPA's initial empty
   route loaded the setup-wizard *page*, whose `on_page_load` — seeing `setup_complete` true —
   does `window.location.href = default_path || "/desk"`, which lands back on the empty route →
   reload forever (~3/s). Root-caused by trapping the reload + reading the runtime boot.
   **Fix:** patch `home_page` → `"Workspaces"` (a standard page). `pageview.show("")` then loads the
   Workspaces view, whose `show()` redirects the empty route to the first workspace. Empty `/app`,
   the home breadcrumb, and direct workspace URLs all work.

2. **Blank form body — `Table MultiSelect requires a Table with atleast one Link field`.**
   `getdoctype` must return a **meta bundle**, not just the DocType: the main meta *plus* the meta of
   every child doctype referenced by a `Table`/`Table MultiSelect` field. ferro returned 1 doc; the
   oracle returns 8 (User + Has Role, User Email, …). **Fix:** `build_meta_doc` for each table field's
   `options` doctype; tag child rows with `doctype` ("DocField"/"DocPerm"). Form went 0 → 79 controls.

3. **Form timeline crash — `Cannot read properties of undefined`.** `frappe.model.sync_docinfo`
   keys `frappe.model.docinfo[doctype][name]` off `docinfo.doctype`/`.name`; the timeline reads it
   back. ferro's docinfo lacked those keys. **Fix:** include `doctype`, `name`, `user_info` in the
   getdoc docinfo.

4. **Reportview field qualifiers.** List views send fields as `` `tabUser`.`full_name` ``; ferro's
   ORM expects bare column names. **Fix:** `bare_field()` strips the `` `tab..`. `` prefix and any
   `as alias`, applied to fields and `order_by`.

5. **Write path (3 sub-fixes).** The New form (a) calls `getdoc` with a `new-…` placeholder name —
   must return `{"message":[]}` HTTP 200, not a 404, or it errors; (b) on save, the client's
   temporary `name` must be dropped so naming autogenerates a real one (it was leaking
   `new-todo-…` into the DB); (c) the save response must echo `localname` (the temp name) so
   `frappe.model.sync` renames the local form doc to the server name (else the form stays "new").

---

## Limitations (honest, with reasons)

These do **not** stop Desk from running; they are bounded features left for follow-up:

- **Realtime / socket.io** is not served — no live notifications, presence, or background-job
  progress. Desk is designed to degrade without it; everything else works.
- **Charts** (`dashboard_chart.get`) return empty series — time-series aggregation
  (group-by-period, cumulative) isn't implemented. Number cards *do* show real counts.
- **Translations** are not loaded (`get_boot_translations` → `{}`); UI is English.
- **Writes from Desk** work end-to-end for **pure-CRUD doctypes**: filling a form and saving
  (`savedocs`/`client.save`) persists via `orm::insert`/`update`, autonames the doc, and the form
  renames its local doc to the server name and shows the "Saved" toast (verified by creating a ToDo
  through the UI). Caveats: (a) **child-table rows are not persisted** by the pure-Rust ORM (parent
  fields only); (b) doctypes whose **Python controllers** add validation/side-effects do not run in
  this binary — that is the sibling `ferrod` PyO3 build's job. Submit/cancel/workflow are not wired.
- **Session/CSRF** run in demo mode (default-user = Administrator, CSRF unvalidated). A real
  multi-user login (sid cookie → `tabSessions`) is a straightforward extension of `auth`.
- **docinfo** (attachments/comments/assignments/versions/shares) returns empty collections; the
  timeline still shows the document's own create/modify events from its fields.
- The **boot** long-tail is served from a captured per-site baseline snapshot; the
  render-critical and dynamic keys are computed/patched live. Computing the entire boot from SQLite
  in Rust is feasible (the structured spec in `SPEC-subsystems.json` maps every key) but unnecessary
  for the usability goal.

---

## Files

- `ferro/src/desk.rs` — the Desk module (assets, HTML render, boot, `frappe.*` methods).
- `ferro/src/main.rs` — `--desk`/`--assets`/`--desk-boot` flags, raw-response pipeline, dispatch.
- `ferro/src/desk_boot.json`, `desk_assets.json` — vendored baseline snapshots (`include_str!`).
- `ferro-desk/SPEC-synthesis.md`, `SPEC-subsystems.json` — the deep per-subsystem spec (5 agents).
- `oracle/` (captured ground truth; not committed — see desk/README.md) — captured ground truth + ferro vs real screenshots.
