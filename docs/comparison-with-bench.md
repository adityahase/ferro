# `ferro` vs `bench`

`ferro` is to Ferro what `bench` is to Frappe: one CLI to install the toolchain, build, create
sites, install apps, serve the REST API, and run the app frontends. The CLI surface is modelled on
`bench` on purpose — but Ferro is a *data-plane* runtime, not a 1:1 Frappe, so the mapping is
partial and deliberate. This document is the full, honest mapping: what corresponds, what Ferro
intentionally does **not** do, and what Ferro adds that `bench` has no analog for.

A note on what `bench` actually is, so the comparison is fair: `bench` is a [click](https://click.palletsprojects.com/)-based
Python CLI, and a *bench* (the workspace it manages) is a directory of `apps/` (cloned app repos,
pip-installed into a virtualenv), `sites/` (per-site config + a database on a **separate** MariaDB/
Postgres server), `env/` (the Python virtualenv), `config/` (redis + supervisor + nginx configs),
and a `Procfile` that `honcho`/`bench start` runs (web via gunicorn, the scheduler, background
workers, socketio, redis, and the asset watcher). Most of what `bench` does is wiring up that
multi-process, multi-server stack. Ferro replaces the *data plane* of one of those processes (the
web worker) with a Rust binary and drops the rest.

---

## Command mapping

The forge (`ferro init`) is bench-layout compatible, so the verbs line up closely. The right-hand
notes are where the truth lives.

| `bench` command | `ferro` equivalent | Notes |
|---|---|---|
| `bench init <dir>` | `ferro init <forge> [--port N]` | Create the workspace. `ferro init` writes `ferro.json`, `apps/ sites/ logs/ config/`, `sites/common_site_config.json` (`webserver_port`, `socketio_port`, `developer_mode`), and a `Procfile`. No virtualenv, no `env/`, no redis/supervisor/nginx config — those don't exist in a forge. |
| `bench new-site <site>` | `ferro new-site <name>` | Ferro decompresses the bundled frappe-core seed (`framework/seed/core.db.gz`, 278 doctypes + Administrator) into `sites/<name>/db/<name>.db` and writes `site_config.json` with `db_type: "sqlite"` and a fresh Fernet-shaped `encryption_key`. **No MariaDB/Postgres/Redis server** — the site database is a single SQLite file. |
| `bench get-app <app>` | `ferro get-app <app> [--git URL] [--branch B] [--mirror DIR]` | `git clone --depth 1` into `apps/`, or copy from a local mirror (`--mirror`, `$FERRO_APP_MIRROR`, or the known on-box clone dir). Resolves a URL/branch from the built-in registry (`ferro.json`) or defaults to `github.com/frappe/<app>`. Unlike `bench get-app`, there is **no pip install / setup.py step** — Ferro never installs the app into a Python environment. |
| `bench install-app <app>` | `ferro install-app <app> [--site S]` | Materialises the app's DocType schema into the SQLite site (via `framework/build_db.py`) and appends to `installed_apps` in `site_config.json`. This is the slice of `bench migrate` that Ferro needs — schema only. |
| `bench migrate` | `ferro install-app` | Schema materialisation only. There is no patch runner, no `bench migrate` "apply all pending patches" semantics, no fixtures sync. |
| `bench build` (bundle Desk/app assets) | `ferro build [ferrod\|ferro/rust\|native\|all]` (runtime) **and** `ferro dev <app>` (frontend) | These are different things sharing a verb. `ferro build` compiles the **Rust runtime** with `cargo` (`ferrod` by default). Frontend assets are not bundled by Ferro at all — app SPAs are served by `vite` via `ferro dev`. There is no Desk asset pipeline. |
| `bench start` | `ferro start` | Runs every process in the forge `Procfile`, multiplexing their output. The default `Procfile` has just `web:` (`ferro serve`). A `bench` Procfile additionally runs the scheduler, workers, socketio, redis and the asset watcher — Ferro's has none of those. |
| `bench serve --port N` | `ferro serve [--port N] [--site S] [--runtime R] [--threads N] [--apps a,b] [--load all\|lazy\|none] [--user U] [--dev]` | Serve the REST API. `bench serve` runs the Werkzeug dev server in front of the full framework; `ferro serve` execs a Rust binary that serves the v1 `/api/resource` surface directly against the site. Port defaults to the forge `webserver_port`. |
| `bench --site S console` / `bench execute` | `ferro request <METHOD> <url> [body] [--site S] [--runtime R] [--apps a,b] [--load L] [--user U] [--dev]` | The closest analog: a single **in-process** REST call (no server). It is not a Python REPL and cannot run arbitrary `frappe.*` code — it issues one HTTP-shaped request and prints the response. |
| `bench --site S migrate` then test | `ferro verify [--site S] [--apps a,b]` | No direct `bench` analog. Functional proof that reads run in Rust and writes drive the **real** app controllers (e.g. hrms `RetentionBonus.validate` rejecting a past date with HTTP 417 and its exact source message). Needs `ferrod` + the apps present. |
| `bench setup requirements` / `bench setup env` | `ferro setup [--yes] [--no-python] [--no-node] [--python-version X]` | Installs the **toolchain** (a C toolchain, git, sqlite, `libjemalloc`, Rust via rustup, a CPython built `--enable-shared` for `ferrod`, Node + Yarn for frontends). `bench setup` instead creates the Python virtualenv and pip-installs app requirements. Different stacks, same "make the box ready" intent. |
| `bench doctor` (process/queue health) | `ferro doctor` | Different scope. `ferro doctor` reports the toolchain (cargo, embeddable Python, jemalloc, node/yarn), which of the three binaries are built, whether the seed DB is present, and the discovered forge. It does **not** check redis/queue/worker health (there are none). |
| `bench setup production` (supervisor + nginx) | — | **No analog by design.** Ferro is a single static binary; there is no supervisor/nginx scaffolding, no gunicorn, no systemd generation. |
| `bench setup supervisor` / `bench setup nginx` | — | Same — intentionally absent. |
| `bench update` | — | **No analog.** No `git pull` of apps + `pip install` + `bench migrate` + asset rebuild orchestration. Update an app by re-running `ferro get-app` / `ferro install-app` and rebuilding the runtime yourself. |
| `bench restart` (supervisor) | — | No process supervisor to restart; `ferro start` runs the Procfile in the foreground. |
| `bench --site S backup` | — | No backup command. The site is a single SQLite file (`sites/<site>/db/<name>.db`); copy it. (`ferro new-site` does create `sites/<site>/private/backups/`, but nothing writes to it.) |
| `bench --site S add-to-hosts` / set-config etc. | — | No host/config sub-CLI. Edit `site_config.json` / `common_site_config.json` directly. |
| (n/a) | `ferro populate [--site S] [--rows N]` | No `bench` analog. Fills read-path doctypes with representative demo rows (default 3000) for measurement. |
| (n/a) | `ferro measure` / `ferro loadtest` (alias `ferro bench`) | No `bench` analog — memory/throughput tooling. See below. |
| (n/a) | `ferro provision-key [user] [--site S]` | Provisions/prints an `api_key:api_secret` for a user (pure-`ferro` runtime). The functional equivalent of generating API keys in Desk, as a CLI. |
| (n/a) | `ferro frontend [app] [--static DIR] [--port N] [--backend-port N]` | Zero-Node fallback: a stdlib reverse-proxy + static server for a pre-built `dist/`. No `bench` equivalent. |
| (n/a) | `ferro bootstrap [--forge DIR] [--site NAME] [--apps a,b] [--no-setup] [--no-build] [--no-populate] [--runtime R]` | One-shot quick start (setup → build → site → apps → ready). Convenience wrapper over `scripts/bootstrap.sh`. |
| `bench version` / `bench --version` | `ferro version` | Prints the Ferro version and which of the three binaries are built. |
| `bench --help` | `ferro <cmd> -h` / `ferro help` | Standard help. |

> The global flag on every `ferro` command is `--forge <dir>` (default: discovered from cwd or
> `$FERRO_FORGE`) — the analog of running `bench` from inside a bench directory. Most commands also
> take `--site <name>` (default: the forge `default_site`, or the sole site if there is exactly one).

---

## Workspace: a *bench* vs a *forge*

A `bench` directory and a Ferro *forge* serve the same role — the per-deployment workspace — and the
forge layout is **deliberately bench-compatible** so that the Frappe apps' own frontend tooling works
unmodified.

A **bench** directory:

```
mybench/
├── apps/<app>/              # cloned app repos, pip-installed into env/
├── sites/
│   ├── common_site_config.json   # { db_host, redis_*, webserver_port, socketio_port, ... }
│   └── <site>/site_config.json   # points at a MariaDB/Postgres database on a DB server
├── env/                     # the Python virtualenv (all app + framework deps)
├── config/                  # redis_cache.conf, redis_queue.conf, supervisor.conf, nginx.conf
├── logs/
└── Procfile                 # web (gunicorn) + scheduler + workers + socketio + redis + watch
```

A **forge** (`ferro init`):

```
myforge/
├── ferro.json                       # forge config: { forge, ferro_home, runtime, default_site, webserver_port }
├── apps/<app>/                      # cloned app repos (with their frontend/)
├── sites/
│   ├── common_site_config.json      # { webserver_port, socketio_port, developer_mode }
│   └── <site>/
│       ├── site_config.json         # { db_type: "sqlite", db_name, encryption_key, installed_apps }
│       └── db/<name>.db             # the SQLite site database (the whole "DB server")
├── logs/
├── config/                          # created, but empty — no redis/supervisor/nginx
└── Procfile                         # web: ferro serve   (only)
```

The load-bearing overlap is intentional:

- **`apps/<app>/`** has the same shape as a bench's, including each app's `frontend/` directory, so
  `ferro dev <app>` can run the app's `vite` dev server straight out of the cloned repo.
- **`sites/common_site_config.json`** carries **`webserver_port`**, which is exactly the key the
  `frappe-ui` vite proxy reads to decide where to send `/api`, `/assets`, `/files`. `ferro dev`
  writes the current backend port into that file before launching vite, so **the app's own proxy
  config works unmodified** — pointed at the Ferro backend instead of a Frappe one.
- **`Procfile`** is honored by `ferro start` the same way `bench start` honors a bench's.

What is *not* shared: a forge has **no `env/` virtualenv** (apps are never pip-installed) and **no
populated `config/`** (no redis/supervisor/nginx). `config/` is created empty for layout parity only.

---

## What `bench` does that `ferro` intentionally does NOT

These are deliberate omissions, not gaps to be filled later. Ferro removes the framework from the
hot path; the surface area below is what comes off with it.

- **No database/cache servers.** `bench new-site` provisions a MariaDB (or Postgres) database and
  uses Redis for cache, queue, and pub/sub. `ferro new-site` writes a single SQLite file. There is
  **no MariaDB/Postgres/Redis** anywhere in a forge — that is the point.
- **No production scaffolding.** No `bench setup production`, no supervisor config, no nginx config,
  no gunicorn, no systemd units. Ferro is a single static binary you run directly.
- **No Python build/install step for apps.** `bench` creates a virtualenv (`env/`) and
  `pip install`s every app and the framework into it. Ferro never does this. `ferro get-app` just
  clones the repo; the apps' Python (on `ferrod`) is imported against Ferro's native-backed `frappe`
  **shim**, not a pip-installed framework.
- **No scheduler, no background workers, no socketio.** A bench `Procfile` runs `bench schedule`,
  `bench worker` (default/short/long queues), and the socketio node process. Ferro's `Procfile` runs
  only `ferro serve`. There is no job queue, no scheduled-job runner, and no realtime/websocket
  server. Framework side-effects that depend on these (email, PDF generation, search indexing,
  realtime events) are deferred by design.
- **No `bench update`.** There is no orchestrated "pull all apps + reinstall deps + migrate +
  rebuild assets" flow. You update by re-fetching/installing apps and rebuilding the runtime.
- **No Desk asset bundling.** `bench build` compiles and bundles the Desk UI and app bundles
  (esbuild). Ferro serves no Desk; `ferro build` compiles the **Rust runtime**, and app SPAs are
  served by `vite` (`ferro dev`) or as a pre-built `dist/` (`ferro frontend`). There is no Desk to
  bundle.
- **No full method/RPC surface.** The Frappe `/api/method/*` RPC surface (and `/api/v2`) is not
  implemented beyond a tiny allow-list (`ping`, `get_logged_user`). SPAs that call bespoke
  whitelisted methods, login, or boot-info endpoints will get 404s for those calls — see
  [limitations.md](limitations.md).

A practical consequence: `ferro start` runs **one** process where `bench start` runs six or more.
The honest framing is that Ferro reproduces the *web data plane* of a bench, not the rest of the
stack around it.

---

## What `ferro` adds that `bench` has no analog for

These are net-new and exist because Ferro's whole premise is footprint.

### Three runtimes, one build verb

`bench` has a single runtime: CPython + the Frappe framework. `ferro build` produces any of three
binaries from one source tree, and `--runtime`/the forge default selects which one `serve`,
`request`, `measure`, and `loadtest` use:

| Runtime | `ferro build` target | What it is | Peak RSS (4 threads) |
|---|---|---|--:|
| **`ferro`** | `ferro build rust` (or `ferro`) | pure-Rust data plane, no Python at all | **~18 MB** |
| **`ferrod`** | `ferro build ferrod` (default) | `ferro` + embedded CPython running the **real** app controllers | **~46 MB** (5 apps, lazy) |
| **`ferro-native`** | `ferro build native` | app controllers **transpiled to Rust**, one binary, zero interpreter | **~18 MB** |

`ferro build all` builds all three (skipping `ferrod` if no embeddable CPython is present). For
reference, a real CPython+Frappe worker against the same site is **~115 MB**.

### Memory & load tooling: `ferro measure` / `ferro loadtest`

There is no `bench` command that reports a worker's memory footprint. Ferro ships two:

- **`ferro measure [--site S] [--runtime R] [--apps a,b] [--load L]`** — post-boot memory
  (RSS/PSS/USS) for a site.
- **`ferro loadtest [--site S] [--threads N] [--rounds N] [--apps a,b] [--load L]`** (alias
  **`ferro bench`**) — drives a write/read load and reports peak memory + throughput.

Both are only available on `ferrod` and `ferro-native`; pure-`ferro` has no `measure`/`loadtest`
subcommand (the CLI tells you to use `--runtime ferrod`/`native`, or to `ferro serve` and load-test
externally). Reproduce the README's numbers with:

```bash
ferro measure  --apps crm,helpdesk,gameplan,hrms,erpnext --load lazy
ferro loadtest --apps crm,helpdesk,gameplan,hrms,erpnext --load lazy --threads 4
```

### Embedded-Python selective dispatch: `--apps` / `--load`

These flags exist only because of `ferrod`'s embedded interpreter and have no `bench` counterpart
(in `bench`, *everything* is always loaded in Python):

- **`--apps a,b`** — which apps' controllers to load into the embedded CPython.
- **`--load all | lazy | none`** (default `lazy`) — when to import controllers. `none` runs purely
  in Rust; `lazy` imports a controller only on first need; `all` imports everything eagerly. This is
  the operative memory lever: per the README, lazy loading takes idle from 50→26 MB and the 4-thread
  peak from 70→46 MB.

On pure-`ferro` and `ferro-native` these flags are inert (there is no interpreter to load apps into),
and `ferro serve`/`request`/`measure`/`loadtest` only forward `--apps`/`--load` when the runtime is
`ferrod`.

### Zero-Node frontend gateway: `ferro frontend`

`ferro dev <app>` is the `bench`-style dev path (vite + Node). `ferro frontend [app]` is a fallback
`bench` has no analog for: a Python-stdlib reverse-proxy + static server that serves a pre-built
`dist/` with SPA fallback and proxies `/api`, `/files`, `/private`, `/method`, `/assets` to the Ferro
backend — useful when Node isn't available or you only have a built frontend.

---

## In one breath

`ferro` mirrors the *verbs* of `bench` (`init`, `new-site`, `get-app`, `install-app`, `start`,
`serve`) and keeps the *workspace layout* compatible so frappe-ui frontends run unmodified — but it
reproduces only Frappe's **web data plane**. The DB/cache servers, the production scaffolding, the
per-app Python install, the scheduler/workers/socketio, `bench update`, and Desk asset bundling are
all intentionally absent. In exchange you get three swappable runtimes and first-class
memory/load tooling — the things that justify Ferro's existence.

See also [cli.md](cli.md) (full command reference), [architecture.md](architecture.md), and
[limitations.md](limitations.md) (what's deferred).
