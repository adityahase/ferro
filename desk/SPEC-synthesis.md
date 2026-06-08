My earlier read of lines 320-445 was stale — the desk wiring DOES exist further down (lines 358-481): `handle()`/`route()` call `desk::try_raw` for assets/HTML/redirect, attach login cookies, and `route_method` calls `desk::route_method`. So the desk subsystem is in fact **fully wired and substantially implemented** in the current ferro tree — much further than the input specs (which were written against an earlier snapshot) claim. 

This changes the report's framing significantly: it's largely a *verification + gap-closing* exercise, not a greenfield build. I have everything I need. Let me write the master report.

# Master Compatibility Report — Running Frappe Desk on the ferro Runtime

## 1. Executive Summary

**Yes — Desk can run on ferro, and the current tree (`src/desk.rs` + the desk wiring in `src/main.rs`) already implements the great majority of it.** The five input specs were written against an *earlier* ferro snapshot and mark most things "missing"; the live source at `/home/frappe/ferro/src/` is materially ahead of those specs. This report reconciles the specs with the **actual current code** and the **oracle ground truth**, and is structured so a Rust dev can finish/verify the work rather than rebuild it.

### Empirical shape of "loading Desk" (from `oracle/requests.json`, 3,422 requests across `/app`, `/app/todo`, `/app/user/Administrator`)

| Category | Count | Notes |
|---|---:|---|
| `/assets/*` (static) | 2,688 | js 1470, svg 753, css 294, woff2 149, mp3 21, png 1 — served from disk |
| `/api/method/*` | 439 | only **16 unique methods** (table below) |
| `/app` & `/desk` HTML | 154 | `/app` → **301** → `/desk` → **200** |
| `/socket.io/*` | 141 | **all 404** — Desk degrades to realtime-off, `pageErrors=0` |
| `/api/resource/*` | **0** | **Desk never uses the REST resource API.** ferro's existing `/api/resource` CRUD is irrelevant to Desk. |

### Architecture (as built today, confirmed correct against the oracle)

1. **Static assets from disk** — `desk::try_raw()` intercepts `/assets/*` before the API router, three-tier resolves via `desk_assets.json` (logical bundle → hashed path), serves bytes with MIME by extension from `<bench>/sites/assets`.
2. **`/app` → 301 → `/desk`; `/desk` → 200 HTML** — `desk.rs` renders an HTML shell (`render_html`) that embeds `frappe.boot = {…}` inline, sets `frappe.csrf_token`, kicks off the translations fetch, and pulls hashed JS/CSS bundles. Matches the oracle's 301/200 flow exactly.
3. **Hybrid bootinfo** — the boot blob is served from a captured snapshot (`src/desk_boot.json`, 131 KB) loadable per-user via `build_boot()`, overridable with `--desk-boot`. This is the pragmatic hybrid the boot spec recommends: snapshot the 40+ static/ignorable keys, keep `user`/`sysdefaults`/`workspaces` correct.
4. **`desk.*`/`client.*` methods mapped onto ferro's ORM** — `desk::route_method()` dispatches all 16 methods Desk calls; reads (`reportview.get/get_list/get_count`, `getdoc`, `getdoctype`, `client.get*`) hit `orm::get_list`/`get_doc`/`count`/`MetaCache`; toolbar/boot methods are stubbed.
5. **Session + login** — `/api/method/login` returns `{message, home_page:"/app"}` and `attach_login_cookies()` sets `sid`/`system_user`/`full_name`/`user_id`/`user_lang`; CSRF currently accepted (demo posture).

### What degrades (acceptable)
- **Realtime** (`/socket.io`) — 404, so no live document/notification push. Confirmed graceful in the oracle (`pageErrors=0`).
- **Client scripts / dashboards / form assets** (`__js`, `__dashboard`, etc.) — stubbed null/empty; forms render, but custom client-side behavior is inert until ferrod's `pyrt` runs controllers.
- **Translations** — `get_boot_translations` returns `{}` (real oracle returns ~country-name map); UI shows untranslated English strings, which is fine for `en`.
- **Notifications counts** — stubbed empty (and not even on the load path; they fire post-socketio).

### The single most important architectural fact
**Desk talks to `/api/method/*` only, never `/api/resource/*`.** The whole compatibility surface for Desk is the 16-method dispatch in `desk::route_method` + asset serving + the `/desk` HTML shell + the boot blob. Get those right and Desk works; the REST resource layer is orthogonal.

---

## 2. Categorized Compatibility Issue List

Tags: **criticality** `[BLOCKER|IMPORTANT|NICE|IGNORABLE]` · **status** (reflects *current* `desk.rs`/`main.rs`, corrected vs the input specs) `[exists|partial|missing]` · **work** `[stub-ok|needs-impl]`.

### A. Static assets + `/desk` HTML + redirect (`desk.rs`)

| Item | Crit | Status | Work | Notes / exact contract |
|---|---|---|---|---|
| `/app` → `/desk` redirect | BLOCKER | **exists** | done | `try_raw`: `path.replacen("/app","/desk",1)`, 301. Oracle: `GET /app → 301`. **Gap:** verify query string is preserved (spec wants `forward_query_parameters`); `/app/todo` must → `/desk/todo`. |
| `/desk` HTML render | BLOCKER | **exists** | needs-verify | `render_html` returns 200 `text/html`, embeds `frappe.boot`, `frappe.csrf_token`, bundle `<script>/<link>`, icon `fetch()`. Oracle: `/desk → 200`. Verify Content-Type header is `text/html; charset=utf-8` on egress (`respond_raw`). |
| `/assets/*` static serving | BLOCKER | **exists** | needs-verify | `serve_asset`: reads `<assets_dir>/<rel>`, MIME by extension. 2,688 reqs. **Verify** MIME map covers js/css/svg/woff2/mp3/png; **verify** symlink traversal (`sites/assets/frappe` → `apps/frappe/frappe/public`) resolves (use `fs::read` which follows symlinks — OK). Add `Cache-Control: immutable, max-age=31536000` for hashed names. |
| `assets.json` resolution | IMPORTANT | **exists** | done | `desk_assets.json` loaded at boot; `asset_map.get(name)` → hashed path; fallback `/assets/frappe/dist/js/{name}`. |
| `dev_server` flag = 0 | BLOCKER | **exists** | needs-verify | Must be falsey in the shell. **If truthy, Desk hangs on the Vite dev server.** Confirm the rendered HTML does not set `window.dev_server = 1`. |
| boot blob embed | BLOCKER | **partial** | needs-impl | `build_boot()` reads `desk_boot.json`. Per-user `user`/`sysdefaults`/`workspaces` should be live; currently snapshot. Compact JSON (no pretty-print). |
| CSRF token in shell | IMPORTANT | **partial** | stub-ok | `frappe.csrf_token = "{csrf}"` emitted; demo uses a fixed/any token. |
| icon SVG `fetch()` wrapper | IMPORTANT | **exists** | done | 4 icon SVGs fetched client-side with `credentials:"same-origin"` (lines 96–145). 753 svg reqs served from disk. |
| sounds / favicon / lang / theme / app_name / build_version | NICE | **partial** | stub-ok | Favicon + logo hard-referenced in shell; others defaultable. |

### B. Bootinfo keys (`desk_boot.json` snapshot + `build_boot`)

| Key | Crit | Status | Work | Notes |
|---|---|---|---|---|
| `user` (name, roles, defaults, perms) | BLOCKER | **partial** | needs-impl | Snapshot today; should be live per session: `tabUser` + `tabHasRole` + `auth::Perm` + `tabSysDefaults`. |
| `sysdefaults` (incl. `setup_complete=1`) | BLOCKER | **partial** | needs-impl | `setup_complete=1` is **mandatory** or Desk routes into setup wizard. Snapshot OK if it contains `setup_complete:"1"`. |
| `workspaces` / `workspace_sidebar_item` | BLOCKER | **partial** | needs-impl | Sidebar. Snapshot renders a sidebar; live requires multi-table join + perm filter. Snapshot acceptable for demo. |
| `page_info` | BLOCKER | **partial** | needs-impl | Role-filtered `tabPage`. Snapshot OK. |
| `single_types`, `nested_set_doctypes`, `tree_view_doctypes` | IMPORTANT | **partial** | needs-impl | Cheap `tabDocType`/`tabDocField` queries; cache in memory. Snapshot OK initially. |
| `assets_json`, `metadata_version`, `apps_data` | IMPORTANT | **partial** | needs-impl | Drive routing/cache-bust. Keep from snapshot. |
| `navbar_settings`, `desk_settings`, `docs`, `doctype_layouts` | IMPORTANT | **partial** | stub-ok | `docs` is ~46 KB; keep snapshot, never recompute live. |
| `modules`, `module_list`, `desktop_icons`, `lang`, `lang_dict`, `notification_*`, `home_page`, `time_zone`, `versions`, … | NICE | **partial** | stub-ok | All present in snapshot or stubbable (`{}`/`[]`/`0`). |
| ~40 ignorable keys (`calendars`, `treeviews`, `letter_heads`, `socketio_port`, `developer_mode`, `read_only`, `max_file_size`, `email_accounts`, `module_app`, …) | IGNORABLE | **partial** | stub-ok | Static/empty; keep from snapshot. |

**Recommendation:** the snapshot-driven boot is sound. The only keys worth promoting to *live* are `user`, `user_info`, `sysdefaults` (for correctness across logins/perms). Everything else stays snapshot.

### C. List-view data stack (`desk::route_method` → ORM)

| Method | Crit | Status | Work | Exact contract (oracle-verified) |
|---|---|---|---|---|
| `frappe.desk.reportview.get` | BLOCKER | **exists** | needs-verify | Empty: `{"message":[]}`. Non-empty: `{"message":{"keys":[…],"values":[[…]],"user_info":{}}}` (compressed). Oracle `reportview_user.json` confirms `keys/values/user_info`. |
| `frappe.desk.reportview.get_list` / `frappe.client.get_list` | BLOCKER | **exists** | needs-verify | `{"message":[ {…}, … ]}` (uncompressed dicts); empty `{"message":[]}`. |
| `frappe.desk.reportview.get_count` / `frappe.client.get_count` | BLOCKER | **exists** | done | `{"message":N}`. Oracle `get_count.json` = `{"message":0}`. |
| field-name stripping (`` `tabToDo`.`name` `` → `name`) | BLOCKER | **partial** | needs-verify | `bare_field()` exists. Verify it strips backticks **and** table prefix for all callers; orm already `rsplit('.')`. |
| `frappe.desk.listview.get_list_settings` | IMPORTANT | **exists** | done | Not found → `{}` (NOT an error). Oracle confirms `{}`. |
| `frappe.model.utils.user_settings.save` | NICE | **partial** | stub-ok | **Mismatch to fix:** ferro returns `message(Value::Null)`; oracle echoes the parsed dict: `{"message":{"updated_on":"…","last_view":"List"}}`. Echo back the parsed `user_settings`. |
| `frappe.model.utils.user_settings.get` | NICE | **exists** | stub-ok | `{"message":"{}"}` (JSON string). |
| `frappe.desk.listview.get_group_by_count` | NICE | **exists** | stub-ok | `{"message":[]}`. |
| `frappe.client.get_value` / `get_single_value` | NICE | **exists** | needs-verify | `get_single_value` oracle = `{"message":0}`. |
| response envelope | IMPORTANT | **exists** | done | `/api/method/*` → `{"message":…}` (RPC). Desk uses only this; `/api/resource` `{"data":…}` is unused by Desk. |

### D. Form-view stack (`desk::route_method`)

| Item | Crit | Status | Work | Contract |
|---|---|---|---|---|
| `frappe.desk.form.load.getdoctype` | BLOCKER | **exists** | needs-verify | `{"docs":[…], "user_settings":"{…}", ["parent_dt":…]}` — **NOT** `{"message":…}`. Verified live: `{"docs":[{"doctype":"DocType","name":"ToDo",…}]}`. Must emit **all** `tabDocType` columns, full `fields[]` (all `tabDocField` cols), `permissions[]` (`tabDocPerm`), and `__*` script keys (stub null). `user_settings` is a **stringified** JSON. |
| `frappe.desk.form.load.getdoc` (existing) | BLOCKER | **partial** | needs-impl | `{"docs":[{…doc…}], "docinfo":{…}, "_link_titles":{}}`. Doc via `orm::get_doc`; docinfo stubs (`attachments:[]`, perms dict, etc.). |
| `getdoc` for `name=new` | BLOCKER | **exists** | done | Oracle: `{"message":[]}` (Desk builds blank client-side). **Note:** the oracle capture shows the whitelisted wrapper `{"message":[]}` for the `new` case — match the captured shape, do not invent a fake doc. |
| DocField full serialization (~45 cols) | IMPORTANT | **partial** | needs-impl | `meta.rs` loads only 6 columns; getdoctype needs all. Expand query or post-query the full `tabDocField` rows. |
| DocPerm array | IMPORTANT | **partial** | needs-impl | `SELECT * FROM tabDocPerm WHERE parent=? ORDER BY idx`, full columns incl `if_owner/select/mask/impersonate`. |
| `get_doc_permissions` | IMPORTANT | **partial** | stub-ok | Stub all-1 for Administrator; real impl via `auth::Perm`. |
| `__js/__css/__dashboard/...` | NICE | **stub-ok** | stub-ok | All null/empty; `__assets_loaded:true`. Not needed until user interacts. |
| `_link_titles` | NICE | **stub-ok** | stub-ok | `{}`. |
| `frappe.desk.form.load.get_docinfo` | NICE | **exists** | done | Stubbed `empty_docinfo()`. |

### E. Session / login / CSRF / boot methods / socket.io (`main.rs` + `desk.rs`)

| Item | Crit | Status | Work | Contract |
|---|---|---|---|---|
| `/api/method/login` | BLOCKER | **partial** | needs-impl | Returns `{message:"Logged In", home_page:"/app"}` + cookies. **Gap:** real password check vs `__Auth` (PBKDF2). Demo accepts default user. |
| `sid` session cookie | BLOCKER | **partial** | needs-impl | `attach_login_cookies` sets `sid/system_user/full_name/user_id/user_lang`. Real impl: resolve user from `tabSessions`. Demo: default-user = Administrator. |
| CSRF validation | IMPORTANT | **partial** | stub-ok | Currently accepted (no enforce). Live oracle **does** enforce (`CSRFTokenError`); ferro demo posture = ignore. Fine for demo. |
| `frappe.auth.get_logged_user` | IMPORTANT | **exists** | done | `{"message":"Administrator"}`. Verified live. |
| `frappe.apps.get_apps` | NICE | **exists** | stub-ok | `{"message":[]}`. Oracle confirms `[]`. |
| `get_all_roles` | IMPORTANT | **exists** | done | Live: sorted role-name array from DB. |
| `get_timezones` | NICE | **exists** | done | `{"message":{"timezones":[…IANA…]}}`. |
| `get_session_default_values` | IGNORABLE | **exists** | stub-ok | `{"message":"[]"}` (stringified). |
| `get_recent_tasks` | IGNORABLE | **exists** | stub-ok | `{"message":[]}`. |
| `get_boot_translations` | IMPORTANT | **partial** | stub-ok | `{"message":{}}`; real oracle returns country-name map. `en` UI fine. |
| setup_wizard `load_languages`/`load_user_details` | IGNORABLE | n/a | stub-ok | **Never called** because `setup_complete=1`. Return `{}` if hit. |
| `get_open_count`/`get_notifications` | IGNORABLE | **exists** | stub-ok | Stubbed empty. **Not on load path** (post-socketio). |
| `/socket.io/*` | IGNORABLE | **exists** | done | `try_raw` 404s. 141 reqs, graceful. |
| `client.get_single_value` | NICE | **exists** | done | Oracle `{"message":0}`. |

---

## 3. Dependency-Ordered Implementation Plan (Milestones)

The desk subsystem is already wired (`main.rs` lines 358–481 call `desk::try_raw` and `desk::route_method`; login cookies attached at 371–395). So these milestones are **finish-and-verify**, in dependency order. Run ferro with `serve <site> --desk --desk-boot src/desk_boot.json` and drive a headless browser against the same oracle scenarios.

### Milestone 0 — Build & wire sanity (prerequisite)
- Confirm `cargo build` (the `ferro` pure-Rust binary, no `python` feature) compiles `desk.rs`.
- Confirm egress: `respond_raw` sends `RawResp.content_type` + status for HTML/bytes/redirect (not forced `application/json`). This is the one path where a wiring bug would silently corrupt HTML/asset responses.
- Confirm `--desk` sets `default_user=Administrator` when no user set (main.rs 258–262).

### Milestone 1 — **Desktop renders** (`/app/` home)
Goal: browser loads `/app`, follows 301 → `/desk`, parses boot, paints the sidebar + home, `pageErrors=0`.
1. `/app`→`/desk` 301 with query preserved; `/desk`→200 `text/html`.
2. Asset pipeline: every `/assets/*` (2,688) returns 200 with correct MIME; verify js/css/svg/woff2/mp3. Add immutable cache headers for hashed names.
3. Boot blob embeds with `dev_server` falsey and `sysdefaults.setup_complete=1`.
4. Boot methods used pre-paint return correct stubs: `get_logged_user`, `get_all_roles`, `get_timezones`, `get_apps`, `get_session_default_values`, `get_recent_tasks`, `get_boot_translations`, `user_settings.save` (echo dict), `get_single_value`.
5. `/socket.io` 404 (confirm no UI hang).
**Exit:** `/desk` paints; console matches `oracle/console.json` (no new page errors).

### Milestone 2 — **List views navigable** (`/app/todo`)
Goal: clicking a doctype list loads rows.
1. `reportview.get` compressed `{keys,values,user_info}`; empty → `{"message":[]}` (exact).
2. `reportview.get_count` → `{"message":N}`.
3. `reportview.get_list`/`client.get_list` uncompressed dicts.
4. `get_list_settings` → `{}` when absent.
5. Field stripping verified for backtick+table-prefix across all three.
6. `user_settings.save` echoes the parsed settings dict (fix the current `Null`).
**Exit:** ToDo list renders rows matching `oracle/reportview_user.json` / `reportview_get.json` shapes.

### Milestone 3 — **Forms navigable** (`/app/user/Administrator`)
Goal: opening a document renders the form.
1. `getdoctype`: full `tabDocType` columns + full `fields[]` + `permissions[]` + `user_settings` (stringified) + `__*` stubs; `{"docs":[…]}` envelope. Cross-check against `oracle/getdoctype_todo.json` / `resp_getdoctype_*.json`.
2. Expand `meta.rs` DocField load to all ~45 columns (or post-query) so getdoctype is complete.
3. DocPerm full-column rows.
4. `getdoc` existing doc: `{"docs":[doc],"docinfo":{…stubs…},"_link_titles":{}}` via `orm::get_doc`. `name=new` → match captured `{"message":[]}`.
5. `get_doc_permissions` stub all-1 (Administrator).
**Exit:** User form renders matching `oracle/getdoc_user.json` / `doc_html.json`; new-doc form opens blank.

### Milestone 4 — Correctness hardening (post-demo)
Promote `user`/`user_info`/`sysdefaults` to live; real password check + `tabSessions` sid resolution; CSRF enforcement; live `workspaces`/`page_info` with perm filtering; populate `get_boot_translations`.

---

## 4. Exact Stubbable Methods (stub JSON table)

All are oracle-confirmed. `desk::route_method` already returns most of these; the **Fix** column flags the one current mismatch.

| Method | HTTP | Stub response | Fix needed? |
|---|---|---|---|
| `frappe.apps.get_apps` | GET | `{"message":[]}` | no |
| `frappe.translate.get_boot_translations` | GET | `{"message":{}}` | no (en) |
| `frappe.core.doctype.session_default_settings…get_session_default_values` | GET | `{"message":"[]"}` (stringified) | no |
| `frappe.core.doctype.background_task…get_recent_tasks` | POST `limit=15` | `{"message":[]}` | no |
| `frappe.core.doctype.user.user.get_timezones` | GET | `{"message":{"timezones":["UTC","America/New_York","Europe/London","Asia/Kolkata","Australia/Sydney"]}}` (or full IANA) | no |
| `frappe.core.doctype.user.user.get_all_roles` | GET | live DB list, fallback `{"message":["Administrator","All","Desk User","Guest","System Manager"]}` | no |
| `frappe.desk.listview.get_list_settings` | POST `doctype=…` | `{}` | no |
| `frappe.desk.listview.get_group_by_count` | POST | `{"message":[]}` | no |
| `frappe.model.utils.user_settings.get` | GET/POST | `{"message":"{}"}` | no |
| `frappe.model.utils.user_settings.save` | POST | `{"message": <echo parsed user_settings dict>}` e.g. `{"message":{"updated_on":"…","last_view":"List"}}` | **YES — currently returns `null`** |
| `frappe.desk.notifications.get_open_count` | POST | `{"message":{"open_count_doctype":{},"open_count_other":{},"targets":{}}}` | no (not on load path) |
| `frappe.desk.notifications.get_notifications` | POST | `{"message":{"open_count_doctype":{},"open_count_other":{},"targets":{},"new_messages":[],"notification_count":{},"notification_percent":{}}}` | no |
| `frappe.desk.page.setup_wizard.setup_wizard.load_languages` | POST | `{}` | no (never called; `setup_complete=1`) |
| `frappe.desk.page.setup_wizard.setup_wizard.load_user_details` | POST | `{}` | no (never called) |
| `frappe.client.get_single_value` | GET | `{"message":0}` | no |
| `frappe.desk.form.load.get_docinfo` | GET | `empty_docinfo()` (perms all-1, all arrays empty) | no |
| `/socket.io/*` | GET | HTTP `404` | no |
| `frappe.desk.form.load.getdoc?name=new` | GET | match capture: `{"message":[]}` | no |
| `__js/__css/__dashboard/__*` in getdoctype | — | `null`/`[]`/`{}`, `__assets_loaded:true`, `__dashboard:{"transactions":[],"non_standard_fieldnames":{},"internal_links":{}}` | no |
| `_link_titles` in getdoc | — | `{}` | no |

---

## 5. Top Gotchas That Will Break Desk If Missed

1. **`dev_server` must be falsey** in the `/desk` shell. If truthy, Desk tries the Vite dev server and hangs. Highest-severity single flag.
2. **`sysdefaults.setup_complete = "1"`** must be in the boot blob, or Desk routes to the setup wizard (then calls the un-stubbed `load_languages`/`load_user_details`). With it set, those endpoints are never hit.
3. **Form-load endpoints use `{"docs":[…]}` / `{"docs",…,"docinfo",…}`, NOT `{"message":…}`.** `getdoctype` and the existing-doc `getdoc` are *unwrapped*. (The `name=new` `getdoc` capture shows `{"message":[]}` — match the captured shape per case; do not blanket-wrap.) Verified live: `getdoctype` → `{"docs":[{"doctype":"DocType","name":"ToDo",…}]}`.
4. **`reportview.get` empty result is `{"message":[]}`, not an empty compressed object.** With rows it's `{"message":{"keys":[…],"values":[[…]],"user_info":{}}}`. Mixing these breaks the list grid. (`oracle/reportview_user.json`, `resp_get_list_0.json`.)
5. **Field names arrive backtick+table-prefixed** (`` `tabToDo`.`name` ``). Every reportview/list/count handler must strip to bare `name` before hitting the ORM. (`bare_field()` must be applied on all paths.)
6. **`user_settings.save` must echo the parsed dict** (`{"message":{…}}`), not `null`. This is the one current code mismatch vs the oracle; Desk persists view state from this response.
7. **`getdoctype` must emit ALL columns** — full `tabDocType` (~55 cols), full `tabDocField` (~45 cols, ordered by `idx`), full `tabDocPerm` rows. `meta.rs` currently loads only 6 DocField columns; a thin field set makes Desk render a broken form. `user_settings` inside getdoctype is a **stringified** JSON value.
8. **Desk uses `/api/method/*` exclusively (0 `/api/resource` calls).** Do not spend effort aligning the REST resource envelope to Desk; it's unused. The method dispatch + `{"message":…}` RPC envelope is the entire data contract.
9. **`respond_raw` must honor `RawResp.content_type`/status.** HTML, asset bytes, and the 301 redirect all flow through the raw path; if anything forces `application/json` or status 200, the shell/assets/redirect silently corrupt.
10. **Asset symlink + MIME correctness.** `sites/assets/frappe` is a symlink into `apps/frappe/frappe/public`; `fs::read` follows it (OK), but the MIME map must cover `.woff2`→`font/woff2`, `.svg`→`image/svg+xml`, `.mp3`→`audio/mpeg`, `.js`→`application/javascript`, `.css`→`text/css`. 753 SVGs are fetched via client-side `fetch()` (icons), so a wrong SVG MIME breaks icons specifically.
11. **CSRF posture is intentionally relaxed** in the demo (ferro accepts any token; the live oracle enforces and returns `CSRFTokenError`). Keep ignore-CSRF for the demo; flag enforcement as M4 hardening. Don't accidentally enable enforcement without rendering a matching token in the shell.
12. **Socket.io 404 is correct and required to be graceful** — don't return 500. Desk's realtime layer falls back cleanly on 404 (oracle `pageErrors=0` across 141 socket.io 404s).

---

### Key file references (all absolute)
- `/home/frappe/ferro/src/desk.rs` — asset serving, `/desk` HTML render, `try_raw`, `route_method` (16 methods), `RawResp`, login cookies.
- `/home/frappe/ferro/src/main.rs` — desk wiring: `App.desk` field, `try_raw` call + login-cookie attach (≈358–395), `route_method`→`desk::route_method` (≈478–481), `--desk`/`--desk-boot` flags (≈240–265), `respond_raw` (≈398).
- `/home/frappe/ferro/src/desk_boot.json` (131 KB snapshot boot), `/home/frappe/ferro/src/desk_assets.json` (bundle→hash map).
- `/home/frappe/ferro/src/meta.rs` — **DocField loads only 6 columns; must expand for getdoctype (M3).**
- `/home/frappe/ferro/src/orm.rs` — `get_list`/`get_doc`/`count` backing the read methods.
- Oracle ground truth: `/home/frappe/ferro-desk/oracle/` — `requests.json` (3422 reqs), `api_bodies.json` (per-endpoint req/resp), `getdoctype_todo.json`, `getdoc_user.json`, `reportview_user.json`, `get_count.json`, `served_boot.json`, `real-*.png` reference screenshots.