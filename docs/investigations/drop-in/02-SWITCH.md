# The ferro ⇄ CPython runtime switch (drop-in)

Goal: inside an **existing, unmodified** Frappe bench, flip the web runtime between CPython
(`gunicorn`/`bench serve`) and ferro with **one reversible change**. Everything non-Python —
`sites/`, `assets/`, the Node `socketio.js`, workers, scheduler, nginx, all client JS/HTML/Jinja —
stays exactly as-is.

Source of truth: `sites/common_site_config.json` → `"web_runtime": "ferro" | "gunicorn"` (default
`gunicorn`). Frappe ignores the unknown key, so adding it is harmless when the runtime is CPython.

## What was built (Phase 0 — landed & verified)

ferro `serve` learned to read the bench layout, so the binary is a launch-swap for gunicorn:

```
ferro serve --bench-mode [--site NAME] [--sites-path PATH] \
            [-b host:port | --port N] [--threads N] [--desk|--no-desk]
```

`--bench-mode` resolves the site exactly like `bench serve`: `--site` → `default_site` in
common_site_config → `currentsite.txt` → single-site fallback; reads `webserver_port`,
`encryption_key` (site_config, falling back to common_site_config), and serves the desk by default.
`-b host:port` is gunicorn-compatible so the prod nginx upstream is byte-for-byte identical.
(`src/main.rs`: `serve()`, `resolve_bench_site()`, `load_encryption_key()`.)

### Verified against the real bench `/home/frappe/benches/bench-cpython314`

```
$ ferro serve --bench-mode --sites-path .../sites -b 127.0.0.1:8011
ferro bench-mode: resolved site -> .../sites/mysite.sqlite
ferro serving .../db/_d3b3bc5c1c1a19aa.db on http://127.0.0.1:8011 (default-user=Administrator, fernet=true, desk=true)

GET /api/resource/User?limit_page_length=2   -> {"data":[{"name":"Guest"},{"name":"Administrator"}]}
POST /api/method/frappe.client.get_count     -> {"message":2}
GET /app                                      -> 301 -> /desk
GET /desk                                     -> 200 text/html 134 KB  (frappe.boot, frappe.csrf_token, window.app, /assets/frappe…)
GET /assets/frappe/dist/css/desk.bundle.*.css -> 200 text/css 634 KB
```

## The dev switch (`bench start`)

`contrib/bench-ferro-switch.sh` toggles the bench's `Procfile` `web:` line (saving the original so
`off` restores it byte-for-byte) and sets the flag:

```
bench-ferro-switch.sh on        # web -> ferro, web_runtime=ferro
bench-ferro-switch.sh off       # web -> bench serve, web_runtime=gunicorn  (exact restore)
bench-ferro-switch.sh status    # show current runtime + binary
bench-ferro-switch.sh prod      # print the supervisor/systemd patch
```

Env: `FERRO_BIN` (default: the repo's `target/release/ferro`; point at `ferrod` for full
faithfulness), `BENCH_DIR`, `FERRO_PORT`, `FERRO_THREADS`. Only the `web:` line changes; `socketio`,
`watch`, `schedule`, `worker` are left intact. Verified ON→OFF restores the Procfile exactly.

## The prod switch (supervisor / systemd)

One gated edit per generator, keyed on `web_runtime`; only the web *program command* changes:

```jinja
{% if web_runtime == 'ferro' %}
command={{ferro_bin}} serve --bench-mode --site {{default_site}} -b 127.0.0.1:{{webserver_port}} --threads {{gunicorn_workers}}
{% else %}
command={{bench_dir}}/env/bin/gunicorn ... frappe.app:application --preload   {# existing #}
{% endif %}
```

Then `bench setup supervisor` (or `bench setup production`) + `bench restart`. Flip back = set
`web_runtime=gunicorn` and re-render. nginx / socketio / workers untouched.

## Why a Procfile/flag swap (and not the alternatives)

- **NOT** intercepting `bench serve` — it isn't a native bench command; bench falls straight
  through to `frappe_cmd`, so intercepting it is *more* invasive than the one-line Procfile edit.
- **NOT** an env-var launcher shim — once ferro has `--bench-mode`, the shim buys nothing.
- **NOT** a `frappe.app` WSGI shim — it keeps the full CPython framework imported, defeating
  ferro's entire ~18 MB reason for existing.

## The binary: ferro vs ferrod

Phase 0 ships the **pure-Rust `ferro`** (no libpython): it serves the REST API, the ~36 native desk
methods, the desk shell and assets — an **Administrator-mode** demo. A *faithful general* drop-in
must use **`ferrod`** (ferro + embedded CPython) so the open-ended `/api/method` whitelist, Jinja
website pages, and credential minting fall through to real Frappe Python. The switch is identical;
only `FERRO_BIN` changes. See `00-REPORT.md` for the tiered-router design and Phases 1–5.

## Honest scope of Phase 0

This is the **launch swap + data-plane/shell serving**, verified. A *logged-in, usable* desk still
needs: Phase 1 real sessions/cookies/CSRF (today identity is process-wide Administrator), Phase 2
the `ferrod` CPython method/website fallthrough, Phase 3 native fidelity fill-ins (submit,
search_link, real docinfo, per-user boot), Phase 4 realtime (`get_user_info` + Redis publish — today
sockets are rejected). See the roadmap in `00-REPORT.md`.
