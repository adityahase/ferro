# Ferro as a Drop-In Runtime Swap Inside an Existing Frappe Bench — Authoritative Synthesis

**Date:** 2026-06-08
**Target bench:** `/home/frappe/benches/bench-cpython314/` (db_type=sqlite, verified)
**Question:** How far is ferro from "flip one switch and the web runtime becomes ferro instead of CPython+gunicorn+frappe.app, while everything non-Python keeps working as-is"?

**Verdict: MODERATE distance.** The data-plane skeleton is real and verified working; four pillars (real sessions, the whitelist tail, Jinja/website, realtime) are missing or fake. The faithful architecture is forced: **ferrod (ferro + embedded CPython) with a tiered router**, not pure-ferro.

---

## 0. The single strategic answer (read this first)

A faithful drop-in **cannot** be pure Rust, because three surfaces are open-ended Python that apps extend arbitrarily:
- the `/api/method/<dotted>` **whitelist** (any app registers methods; only `frappe.handler.execute_cmd` + `is_whitelisted` is faithful),
- **Jinja website rendering** (templates run arbitrary `frappe.*` via `safe_exec`, 911 lines of globals),
- **credential minting** (2FA, OAuth, LDAP, argon2, auth hooks).

Therefore the only viable drop-in is **ferrod**, running a **tiered router** that mirrors `app.py`'s dispatch order (cmd → /api → /backups → /private/files → /.well-known → website → NotFound):

1. **Native Rust fast-path** for the hot understood surface: resource CRUD, list/read methods, assets, the desk shell, sessions/CSRF, realtime publish, cache invalidation.
2. **PyO3 fallthrough** to `frappe.handler.execute_cmd` / `frappe.api.handle` / `frappe.website.serve.get_response` / `frappe.auth` for the long tail.

The hot path stays GIL-free Rust; CPython is touched one-time at boot (import controllers, build the whitelist registry) and per-request only for tail methods. **Pure-ferro remains a deployment MODE** for curated/simple sites that declare website pages and the method tail out of scope — not the default.

Memory: pure-ferro ≈ 18 MB, ferrod ≈ 63 MB (lazy), CPython ≈ 115 MB (per prior measured memory). ferrod still beats CPython substantially while buying faithfulness.

---

## 1. What is REAL and drop-in TODAY (verified)

- **/api/resource/* CRUD** with full perms / if_owner / permlevel ACL — the strongest-covered class (`main.rs:499-605`).
- **~36 native desk methods** in `desk.rs route_method` (verified arms): `reportview.get/get_list/get_count` (correct `{keys,values,user_info}` shape), `client.get/get_list/get_value/get_single_value/get_count`, `form.load.getdoctype/getdoc`, **`form.save.savedocs`** (exists — see §5), `get_desktop_page`, `get_workspace_sidebar_items`, `getpage`, `get_all_roles`, `get_timezones`.
- **API-token + Basic + Fernet auth** — real and drop-in (`auth.rs:81`, `crypto.rs` full Fernet decrypt).
- **Static /assets serving** with traversal guard + long cache (`desk.rs:256-274`).
- **/app→/desk 301** and a workable desk shell.
- **The bench site layout** — VERIFIED: running ferro against `/home/frappe/benches/bench-cpython314/sites/mysite.sqlite` reads the real DB (tabDocType=278) with zero changes to sites/. db_type=sqlite confirmed.

---

## 2. Dimension-by-dimension (8 finders, verifier-weighted)

### 2.1 HTTP request surface & routing — YELLOW, ~40%
ferro's router is a **flat dispatcher** (`main.rs route()` 442-488), no werkzeug Map. Verified: matches only `segments[1] in {method, resource}`; everything else 404s.
- **CRITICAL:** no generic `/api/method/<dotted>` whitelist dispatch — only the curated allowlist; any other method → 404 (verified `main.rs:495`, `desk.rs:416-419`). Oracle shows live misses (`setup_wizard.load_languages` 120×, `load_user_details` 11×).
- **HIGH:** zero `/api/v2/*` (and no explicit `/api/v1/*` mount); v2 has a different route table and `errors[]`/`messages`/`has_next_page` envelope. (Verifier nit: `http_status_code` is NOT v2-specific.)
- **HIGH:** website catch-all 404s (see §2.4). Bare `/` returns a JSON banner, not the home page.
- **HIGH:** file/util routes absent (upload_file multipart, /files, /private/files, /backups, /.well-known); ferro reads the body as a single UTF-8 string (`main.rs:345-354`), so binary/multipart **corrupts**.
- **MEDIUM:** no cmd= legacy RPC; no OPTIONS/CORS preflight or CORS headers.
- **LOW:** envelope/header divergences (no X-Frappe-Request-Id, default Cache-Control, rate-limiter headers).

### 2.2 Sessions, auth, cookies, CSRF — YELLOW, ~35% (THE make-or-break)
- **CRITICAL (verified):** sid is **minted-and-discarded**. ferro never reads the incoming Cookie header (I confirmed: only Set-Cookie on login at `main.rs:370-392`; identity comes from `default_user`, forced to Administrator in desk mode). No real session: no multi-user, no logout, no expiry.
- **CRITICAL (verified):** `/api/method/login` (`desk.rs:353-357`) returns "Logged In" **unconditionally** — no pbkdf2_sha256 check against `__Auth`. Security hole.
- **HIGH (verified):** CSRF token is **per-render random** (`desk.rs:284`), never stored, **never validated** on any unsafe method.
- **MEDIUM:** cookie attribute mismatches (no Max-Age → session cookie dies on close; no user_image; no Secure/encoding; identity cookies not refreshed on resume).
- **MEDIUM:** no logout/session invalidation, no `session_expired` signaling.

The clean design: ferro **reuses Frappe's existing session store** — read/write `tabSessions` directly on the shared SQLite DB (56-hex sid, sessiondata incl csrf_token). Redis is optional (Frappe falls back to DB on cache miss). This gives **bidirectional interop** with a CPython worker, essential for gradual rollout/fallback. Credential minting (2FA/oauth/argon2/hooks) stays Python.

### 2.3 Desk boot & page rendering — YELLOW, ~55%
- **CRITICAL (verified):** boot is a **frozen Administrator snapshot** (`desk_boot.json`, 27 roles, full can_* maps). For non-Administrator users only `user.name` is patched — so every user gets Administrator's roles/workspaces/page_info in the UI (privilege display + staleness). NOTE: this is a UI-fidelity/correctness issue, NOT the server-side authorization boundary (ferro's ORM/perm enforcement on actual API calls is separate).
- **REFUTED (verified):** the `getpage` "_dynamic_page TypeError that blanks the desk" does NOT occur on the real boot path. I confirmed: zero `_dynamic_page` anywhere in the oracle, `consoleErrors: []` in ferro-diag, and **0 getpage calls** in the 3422-request capture. The genuine defect is latent/low: `method_getpage` returns `{message:{...}}` instead of `{docs:[pageDoc]}` and reads tabWorkspace instead of the Page doctype — reclassify HIGH→LOW.
- **MEDIUM:** pure-Rust shell is a `format!` clone of `desk.html` that silently drifts from hooks; CSRF cosmetic.
- **LOW:** frozen `assets_json` goes stale after `bench build` (lazy bundles 404); missing `/apps` redirect; empty navbar settings; build_version hardcoded.

### 2.4 Static assets, website pages & Jinja — YELLOW, ~35%
- **HIGH (verified):** **NO website renderer** — no path_resolver, no TemplatePage/DocumentPage/WebFormPage/StaticPage/ListPage. `/`, `/login`, `/about`, portal, web views all return the API banner or 404. (Note: even the simplest `www/*.html` goes through Jinja + base template + CSRF injection — there is no raw-file shortcut; StaticPage is binary-only.)
- **HIGH (verified):** **NO Jinja engine** — Cargo.toml has no minijinja/tera/askama (verified). The desk shell is a Rust `format!` string, not a render of `www/desk.html`. Framework-coupled globals (`include_script/style/icons`, `safe_exec` 911 lines) make a Rust shim large; DB-stored Web Pages can execute arbitrary `frappe.*`. (Verifier nit: base.html is 124 lines, ~43 tags, no `extends`/`macro` — the "57 constructs" figure is overstated but the coupling point stands.)
- **MEDIUM:** asset serving lacks ETag/Last-Modified/304 and /files; **prod uses nginx for /assets and /files anyway**, so this is **dev-only** (`bench serve`).
- **MEDIUM/LOW:** boot snapshot + cosmetic CSRF; build_version hardcoded.

Three layers: (A) static assets — nginx in prod (ferro need not serve), ferro in dev; (B) desk shell — pure-ferro can own IF it renders real desk.html via minijinja + live boot; (C) arbitrary website — **irreducibly Python → ferrod's `frappe.website.serve.get_response`**.

### 2.5 Socket.IO / realtime — YELLOW, ~5%
socketio.js (Node) + Redis stay byte-for-byte. The swap touches only the two HTTP callbacks + the Redis events channel.
- **CRITICAL (verified):** ferro doesn't expose `frappe.realtime.get_user_info` → EVERY socket connection is **rejected** (ferro's err() lacks a `message` key, so authenticate.js throws and calls `next(new Error('Unauthorized'))`). Realtime is 100% dead, not degraded.
- **CRITICAL (verified):** no sid/session resolution → even with a handler ferro can't map sid→user (only Authorization is read).
- **HIGH:** ferro must IGNORE the X-Frappe-Socket-Secret (it has no Redis secret to compare; comparing → returns `{}` → fails). (Verifier reframing: this is a design constraint on the not-yet-built path; socketio.js DOES require Redis as a relay regardless.)
- **HIGH (verified):** no server→client push — ferro never publishes to the Redis `events` channel, so list_update/doc_update/msgprint/progress never reach the browser. (Must publish **after_commit**, plain JSON.)
- **MEDIUM:** `has_permission` room-gating missing.

### 2.6 Bench/setup integration & THE SWITCH — GREEN, ~70%
ferro already reads the real bench site layout (verified live). Gaps: (1) reads no `common_site_config.json` / default_site / webserver_port (verified empty grep); (2) positional path, not `--site`; (3) binds 0.0.0.0 not 127.0.0.1; (4) prod needs Host / X-Frappe-Site-Name multi-site resolution (verifier correction: prod gunicorn resolves site from the nginx-injected header, NOT default_site). The switch is genuinely tiny — see §3.

### 2.7 DB / Redis / workers / scheduler parity — YELLOW, ~35%
- **REFUTED (verified):** SQLite "incompatible WAL journal modes" is a **non-issue**. journal_mode is persisted in the file header; I confirmed the real site DB header is `0202` (WAL) with live `-wal`/`-shm`. ferro sets no journal_mode PRAGMA (verified), so it **inherits** WAL and cannot revert it. The 1-line WAL PRAGMA is harmless defense-in-depth only. Reclassify HIGH→non-blocker.
- **HIGH (verified):** cross-process **cache invalidation dropped** — ferro writes never DEL `{db_name}|key` or publish `__redis__:invalidate`, so unchanged Python workers serve stale docs/settings; ferro's MetaCache has no invalidate hook (serves stale meta after DDL until LRU eviction). (Precision: ferro has no per-doc cache, so it never serves its own stale doc body — the risk is workers + ferro's MetaCache.)
- **HIGH (verified):** realtime publish missing (same as §2.5).
- **MEDIUM:** SQLite-only — fine for the v1 target (db_type=sqlite); MariaDB/Postgres is XL out-of-scope.
- **MEDIUM:** enqueue from Rust needs RQ pickle — prefer routing enqueue-bearing endpoints to ferrod.

### 2.8 Whitelisted method coverage (the long tail) — YELLOW, ~55%
- **REFUTED (verified):** "savedocs unimplemented / Save dead" is FALSE — `desk.rs:409` → `method_savedocs` → `orm::insert/update`, returning the correct `{docs:[...], _server_messages}` shape. **Re-scope to MEDIUM.** Real defects (verified): **Submit** broken (`form.save.submit` has NO arm → 404), `action`/docstatus ignored, **child-table rows not persisted in pure-Rust** (route those to ferrod's dispatch_write).
- **HIGH (verified):** **no CPython method-dispatch fallback** — the shim has `dispatch_write` (line 338) but **NO `dispatch_method`** (verified grep), and ferrod.rs route() 404s every method except ping/get_logged_user. The native-fast-path + CPython-fallback plan is only half-built. Prerequisites exist (`frappe.whitelist` sets `_is_whitelisted`; controllers imported by `ferro_boot.load`).
- **HIGH (verified):** **search_link/search_widget missing** → every Link/Dynamic Link field autocomplete and link-selector dialogs break (verified: zero search arms). (Verifier nit: the global awesomebar actually uses `global_search.search`, a different missing method — but Link fields alone justify HIGH.)
- **MEDIUM:** `get_docinfo` is an empty stub (sidebar/timeline/attachments/assignments/tags blank) — all plain table reads ferro can do natively.
- **MEDIUM:** `run_doc_method` unimplemented (custom form buttons) — belongs in the CPython fallback.
- **LOW:** boot-adjacent stubs (translations empty, session defaults empty, user_settings not persisted).

---

## 3. THE SWITCH (recommended, minimal-modification)

**One `web_runtime` flag in `common_site_config.json`** (default `"gunicorn"`), plus ferro learning bench site-resolution.

1. **ferro side (~50 lines):** add `--bench-mode`/`--site`/`--sites-path`/`-b host:port` to `serve()` (`main.rs:217-254`); in bench-mode read `common_site_config.json` for default_site/webserver_port/encryption_key and reuse the proven resolvers (`main.rs:59-116`); honor `-b 127.0.0.1:PORT` (replace hardcoded `0.0.0.0` at `main.rs:278`); resolve per-request site from X-Frappe-Site-Name/Host for prod multi-site.
2. **DEV (one Procfile line):** `web: ./ferro serve --bench-mode -b 127.0.0.1:8000 --threads 5 --desk`. Flip back = restore the line.
3. **PROD (one config flag + two template if-blocks):** add `web_runtime` read in `bench/config/supervisor.py` + `systemd.py`; wrap the web command in `supervisor.conf` and `frappe-bench-frappe-web.service` with `{% if web_runtime == 'ferro' %}…ferro serve…{% else %}…gunicorn frappe.app:application --preload…{% endif %}`. Flip back = set flag + re-render.

**Rejected alternatives:** (B) intercepting `bench serve` — it's not a native command, more invasive; (C) env-var + shim — redundant once --bench-mode exists; (D) WSGI proxy shim — keeps frappe imported, defeats the memory goal; (E) pure-ferro as default — closed-world only, can't serve arbitrary apps (keep as a MODE).

Nothing in sites/, assets/, nginx, socketio, workers, or scheduler is touched — reverting is a process-launch change only.

---

## 4. Roadmap (staged)

- **Phase 0 — switch plumbing:** ferro argv/common_site_config parity + flip dev Procfile. Unblocks an Administrator-mode demo serving the real bench. (≈current state + argv.)
- **Phase 1 — real sessions/auth/CSRF (make-or-break):** parse Cookie, verify pbkdf2_sha256, write/read tabSessions (DB-only), persist+validate CSRF, route 2FA/oauth/argon2 to ferrod. Unblocks multi-user, logout, security, bidirectional interop.
- **Phase 2 — ferrod dispatch_method fallback (unlocks the tail):** write `ferro_boot.dispatch_method` (get_attr + is_whitelisted + frappe.call) + wire ferrod.rs fallthrough; delegate website to `frappe.website.serve.get_response`; login to `frappe.auth`. Unblocks run_doc_method, reports, upload, setup_wizard, ALL website/portal/login pages, controller-bearing save/submit, arbitrary apps. **Largest chunk; converts demo→drop-in.**
- **Phase 3 — native fidelity fill-ins (~6 arms):** save.submit + child tables, search_link/search_widget, real get_docinfo, user_settings persist, native translations, per-user get_bootinfo. Unblocks a usable Rust-fast-path desk for pure-CRUD doctypes.
- **Phase 4 — realtime + shared-backend coherence:** get_user_info + has_permission (reuse Phase-1 sid; ignore socket secret); thin write-only Redis client for events PUBLISH (after_commit) + cache DEL/invalidate + MetaCache invalidate hook. Unblocks live updates + cross-runtime coherence.
- **Phase 5 (out of v1) — MariaDB/Postgres** behind a Db trait + SQL emitter. XL, deferred.

---

## 5. Refuted / corrected blockers (downweighted by verifiers, re-confirmed by me)

| Claim | Original | Reality |
|---|---|---|
| SQLite WAL incompatible journal modes | HIGH | **REFUTED** — header `0202`+live -wal/-shm; ferro inherits persisted WAL (sets no PRAGMA). 1-line WAL is defense-in-depth only. |
| getpage `_dynamic_page` blanks the desk | HIGH | **REFUTED** — 0 getpage calls in oracle, empty consoleErrors, no `_dynamic_page`. Latent/LOW (wrong wire shape + doctype). |
| savedocs unimplemented / Save dead | CRITICAL | **REFUTED** — exists (`desk.rs:409`). Real gaps: Submit (no arm→404), child-table writes, docstatus/action ignored → MEDIUM. |

All other blockers were **confirmed** by their verifiers and spot-checked here.

---

## 6. The Python boundary (clean fork line)

- **Stays Python (one-time/control plane, unchanged):** new-site, migrate, build, get-app, install-app, backup, schema DDL, db_type dispatch, nginx config gen. ferro never authors/migrates schema; it consumes assets.json read-only.
- **Stays infrastructure (unchanged):** Node socketio.js; Redis (redis_queue/redis_socketio). Python `bench worker` + `bench schedule` keep running off Redis; ferro is only a job PRODUCER.
- **Becomes ferro (native hot path):** resource CRUD; ~36 desk methods; pure-CRUD saves (~53% of doctypes per prior memory); /assets (+/files dev); /app→/desk + shell; cookie parse + sid→tabSessions resolution; CSRF compare; pbkdf2 verify; write-only Redis (invalidate + events publish); realtime auth endpoints.
- **Must be ferrod CPython (the boundary that forces ferrod):** open-ended whitelist (execute_cmd + is_whitelisted), website/Jinja (`get_response`), controller-bearing save/submit (dispatch_write), credential minting (login/2FA/oauth).
- **Gray areas:** `frappe.enqueue` (prefer routing to ferrod over Rust pickle), extend_bootinfo/boot_session hooks (ferrod), custom search get_query (ferrod fallback). Rule: no-Python-needed → ferro; any-Python-needed → ferrod.

---

## 7. Bottom line

The switch mechanism is **easy and small** (one Procfile line + one flag + two template blocks); ferro already speaks the bench layout. The **distance is in faithfulness**, gated by four items: real sessions (Phase 1), the ferrod method/website fallback (Phase 2), native fidelity fill-ins (Phase 3), and realtime/coherence (Phase 4). Pursue **ferrod with a tiered router** as the drop-in; keep **pure-ferro** as a closed-world mode for simple sites. With Phases 0-2 a logged-in desk loads and basic CRUD works against the real SQLite bench; Phases 3-4 make it genuinely usable and coherent with the unchanged Python workers.