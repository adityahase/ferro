# `ferro` — CLI reference

`ferro` is the command line for the Ferro runtime, modelled on Frappe's `bench`. It is a single
Python 3 script at [`cli/ferro`](../cli/ferro) that drives the compiled Rust runtime binaries, the
schema/seed tooling, and the app frontends.

A **forge** is a workspace (the analog of a bench): it holds your apps, sites, logs and a Procfile,
in a layout that is deliberately bench-compatible so the Frappe app frontends' own tooling (the
frappe-ui vite proxy) works unmodified.

Run `ferro` with no command to print the help; run `ferro <command> -h` for per-command help.

> Honesty note: this document describes only what `cli/ferro`, `scripts/setup.sh` and
> `scripts/bootstrap.sh` actually implement. Flags accepted by the underlying runtime binaries
> (`ferro` / `ferrod` / `ferro-native`) beyond those the CLI forwards are out of scope here.

## Quick start

```sh
ferro setup                         # install toolchain deps (rust, python, jemalloc, node)
ferro init myforge && cd myforge    # create a forge
ferro build                         # compile the runtime (ferrod by default)
ferro new-site dev.localhost        # create a site from the frappe-core seed
ferro get-app crm                   # fetch an app
ferro install-app crm               # materialise its schema into the site
ferro serve                         # serve the REST API
ferro dev crm                       # run the app's frontend (vite) against ferro
```

## The three runtime binaries

Several commands take a `--runtime` selector. The CLI normalises these names
(`_runtime_choice` / the `build` target map):

| You type | Binary built / run | How it is built |
| --- | --- | --- |
| `ferro` or `rust` | `ferro` | `cargo build --release --bin ferro` |
| `ferrod` (default) | `ferrod` | `cargo build --release --features python --bin ferrod` (needs an embeddable CPython) |
| `native` or `ferro-native` | `ferro-native` | `cargo build --release --features native --bin ferro-native` (transpiles controllers first) |
| `all` | all three | builds `rust`, then `ferrod`, then `native` |

The forge's default runtime is `ferrod` (set in `ferro.json` at `init`). Pure `ferro` has no
embedded Python; `ferrod` embeds CPython to run real app controllers; `ferro-native` transpiles the
controllers to Rust ahead of time.

---

## Global flag

| Flag | Default | Meaning |
| --- | --- | --- |
| `--forge <dir>` | discovered from cwd / `$FERRO_FORGE` | The forge directory to operate on. |

Forge discovery (`find_forge`): use `--forge` if given, else `$FERRO_FORGE`, else walk up from the
current directory to the nearest ancestor containing a `ferro.json` whose `"forge"` key is truthy.

---

## Commands

### `ferro setup`

Install the toolchain dependencies (rust, python, jemalloc, node/yarn). `exec`s
[`scripts/setup.sh`](../scripts/setup.sh).

| Flag | Default | Meaning |
| --- | --- | --- |
| `--yes` | off | Pass `--yes` to `setup.sh` (non-interactive). |

```sh
ferro setup --yes
```

Invokes: `bash <ferro_home>/scripts/setup.sh [--yes]`.

> `setup.sh` itself accepts more flags than the CLI forwards — see
> [setup.sh flags](#setupsh-flags) below. To use them, run the script directly.

### `ferro bootstrap`

One-shot quick start: setup → build → init → new-site → get-app/install-app → populate. `exec`s
[`scripts/bootstrap.sh`](../scripts/bootstrap.sh).

| Flag | Default | Meaning |
| --- | --- | --- |
| `--forge <dir>` | `forge` | Forge dir to create (forwarded as `--forge`). |
| `--apps <csv>` | (script default `crm,helpdesk,gameplan,hrms,erpnext`) | Comma-separated apps (forwarded as `--apps`). |
| `--no-setup` | off | Skip the dependency install step (forwarded as `--no-setup`). |

```sh
ferro bootstrap --forge demo --apps crm,helpdesk --no-setup
```

Invokes: `bash <ferro_home>/scripts/bootstrap.sh [--forge DIR] [--apps CSV] [--no-setup]`.

> The CLI only forwards the three flags above. `bootstrap.sh` also accepts `--site`, `--runtime`,
> `--no-populate`, `--no-build` if you run the script directly — see
> [bootstrap.sh flags](#bootstrapsh-flags).

### `ferro doctor`

Report on the toolchain + built binaries. No flags (uses the global `--forge`). Prints: ferro home,
cargo (+ version), embeddable Python (+ libdir), jemalloc, node/yarn, runtime dir, which of
`ferro`/`ferrod`/`ferro-native` are built, whether the seed db is present, and the discovered forge.

```sh
ferro doctor
```

Invokes: nothing external beyond probing `cargo --version`; pure introspection.

> `ferro doctor-json` is a hidden alias mapped to the same function (`cmd_doctor`); it currently
> prints the same human-readable output.

### `ferro build [runtime]`

Compile a runtime binary with cargo.

| Positional / flag | Default | Meaning |
| --- | --- | --- |
| `runtime` (positional) | `ferrod` | One of `ferrod`, `ferro`/`rust`, `native`, `all`. |
| `--transpile` | off | (native only) Re-transpile controllers to `generated.rs` before building. |

Behaviour:
- Requires `cargo` (from `PATH` or `~/.cargo/bin/cargo`); dies with "run `ferro setup`" if absent.
- `ferrod` requires an embeddable CPython. If none is found: in `all` mode it warns and skips
  ferrod; otherwise it dies telling you to run `ferro setup` or set `PYO3_PYTHON`. When found it
  sets `PYO3_PYTHON` for the cargo invocation.
- `native` transpiles first if `src/native/generated.rs` is missing or `--transpile` is
  given. Transpile runs `python <ferro_home>/transpile/transpile.py` with `FERRO_REPOS` set to the
  forge's `apps/` (or `$FERRO_REPOS`).

```sh
ferro build              # builds ferrod
ferro build native --transpile
ferro build all
```

Invokes: `cargo build --release ...` in `<ferro_home>`; for native, also
`python transpile/transpile.py`.

### `ferro init <name>`

Create a new forge (workspace).

| Positional / flag | Default | Meaning |
| --- | --- | --- |
| `name` (positional) | (required) | Forge directory to create. |
| `--port <int>` | `8000` | Default webserver port (also used for `common_site_config.json` and the Procfile). |

Creates `apps/`, `sites/`, `logs/`, `config/`; writes `ferro.json` (with `forge: true`,
`runtime: "ferrod"`, `default_site: null`, `webserver_port`), `sites/common_site_config.json`
(`webserver_port`, `socketio_port = port+1`, `developer_mode: 1`), and a `Procfile` with a `web:`
line. Dies if the target exists and is non-empty.

```sh
ferro init myforge --port 8001 && cd myforge
```

Invokes: nothing external; filesystem + JSON only.

### `ferro new-site <name>`

Create a site from the frappe-core seed (`framework/seed/core.db.gz`).

| Positional / flag | Default | Meaning |
| --- | --- | --- |
| `name` (positional) | (required) | Site name, e.g. `dev.localhost`. |

Decompresses the seed into `sites/<name>/db/_<hex>.db`, writes `site_config.json`
(`db_type: "sqlite"`, a random `db_name`, a fresh Fernet-shaped `encryption_key`,
`installed_apps: ["frappe"]`), creates `private/backups` and `public/files`, and sets the forge's
`default_site` if none is set. Dies if the site exists or the seed db is missing.

```sh
ferro new-site dev.localhost
```

Invokes: nothing external; gzip decompress + JSON. Seeds 278 frappe-core doctypes + an
`Administrator` user (per the on-screen message).

### `ferro get-app <app>`

Fetch an app repo into the forge's `apps/` (git clone, or copy from a local mirror).

| Positional / flag | Default | Meaning |
| --- | --- | --- |
| `app` (positional) | (required) | App name, e.g. `crm`. |
| `--git <url>` | registry entry, else `https://github.com/frappe/<app>.git` | Git URL override. |
| `--branch <name>` | registry entry, else clone default | Branch to clone. |
| `--mirror <dir>` | — | Copy from a local mirror dir instead of cloning. |

Source resolution order: `--mirror`, then `$FERRO_APP_MIRROR`, then the on-box clones at
`/home/frappe/ferro-apps-investigation/repos`, then `git clone --depth 1` (adding `--branch` when a
branch is known). Mirror copies ignore `.git`, `node_modules`, `__pycache__`. Skips with a warning
if the app is already present; dies if git is needed but missing.

```sh
ferro get-app crm
ferro get-app crm --mirror /path/to/local/mirrors
ferro get-app crm --git https://github.com/me/crm.git --branch develop
```

Invokes: `git clone --depth 1 [--branch B] <url> <dest>` (or a `shutil.copytree` from the mirror).

### `ferro install-app <app>`

Materialise an app's DocType schema into a site (the ferro analog of `bench migrate`).

| Positional / flag | Default | Meaning |
| --- | --- | --- |
| `--site <name>` | forge `default_site` | Target site. |
| `app` (positional) | (required) | App to install (must already be in `apps/`). |

Runs the schema builder against the resolved site db, then appends the app to
`site_config.json` → `installed_apps`. Dies if the app is not in `apps/` (telling you to
`ferro get-app` first) or the site has no db.

```sh
ferro install-app crm --site dev.localhost
```

Invokes: `python <ferro_home>/framework/build_db.py --db <site.db> --repos <apps_dir> --apps <app>`
with `FERRO_REPOS` set to `apps/`.

### `ferro populate`

Fill read-path doctypes with representative demo rows.

| Flag | Default | Meaning |
| --- | --- | --- |
| `--site <name>` | forge `default_site` | Target site. |
| `--rows <int>` | `3000` | Number of rows. |

```sh
ferro populate --site dev.localhost --rows 5000
```

Invokes: `python <ferro_home>/framework/populate_demo_data.py --db <site.db> --rows <n>`.

### `ferro serve`

Serve the REST API for a site. Replaces the CLI process with the runtime binary (`execvpe`).

| Flag | Default | Meaning |
| --- | --- | --- |
| `--site <name>` | forge `default_site` | Site to serve. |
| `--runtime <r>` | forge `runtime` (`ferrod`) | `ferrod` \| `ferro`/`rust` \| `native`. |
| `--apps <csv>` | — | (ferrod only) comma apps to load; forwarded as `--apps`. |
| `--load <mode>` | `lazy` | (ferrod only) `all` \| `lazy` \| `none`; forwarded as `--load`. |
| `--port <int>` | forge `webserver_port`, else `8000` | Listen port. |
| `--threads <int>` | (runtime default) | Worker threads; forwarded as `--threads` only if set. |
| `--user <name>` | `Administrator` | Default user (bypasses token auth); forwarded as `--default-user`. |
| `--dev` | off | Expose raw error text; forwarded as `--dev`. |

Notes: the CLI passes `--default-user` (accepted by all three runtimes' `serve`), not `--user`.
`--apps`/`--load` are forwarded only when the runtime is `ferrod`.

```sh
ferro serve --runtime ferrod --apps crm,helpdesk --load lazy --port 8000
```

Invokes (ferrod example):
`ferrod serve <site> --port <p> [--threads N] --default-user Administrator --apps <csv> --load lazy [--dev]`.

### `ferro request <method> <url> [body]`

Issue a single in-process request (no server). `execvpe`s the runtime binary's `request` subcommand.

| Positional / flag | Default | Meaning |
| --- | --- | --- |
| `method` (positional) | (required) | HTTP method (upper-cased by the CLI). |
| `url` (positional) | (required) | Request path, e.g. `/api/resource/CRM Deal`. |
| `body` (positional, optional) | — | Request body (appended only if given). |
| `--site <name>` | forge `default_site` | Site. |
| `--runtime <r>` | forge `runtime` | `ferrod` \| `ferro`/`rust` \| `native`. |
| `--apps <csv>` | — | (ferrod only) forwarded as `--apps`. |
| `--load <mode>` | `lazy` | (ferrod only) forwarded as `--load`. |
| `--user <name>` | `Administrator` | Forwarded as `--user`. |
| `--dev` | off | Forwarded as `--dev`. |

```sh
ferro request GET '/api/resource/CRM Deal?fields=["name"]&limit_page_length=2'
ferro request POST '/api/resource/ToDo' '{"description":"hi"}' --dev
```

Invokes: `<binary> request <site> <METHOD> <url> [body] [--apps ...] [--load ...] --user <u> [--dev]`.

### `ferro measure`

Report post-boot memory (RSS/PSS/USS) for a site. **Only `ferrod` / `ferro-native`** expose
`measure`; pure `ferro` dies with a message telling you to use `--runtime ferrod` (or `native`).

| Flag | Default | Meaning |
| --- | --- | --- |
| `--site <name>` | forge `default_site` | Site. |
| `--runtime <r>` | forge `runtime` | `ferrod` or `native` (not pure `ferro`). |
| `--apps <csv>` | — | (ferrod only) forwarded as `--apps`. |
| `--load <mode>` | `lazy` | (ferrod only) forwarded as `--load`. |

```sh
ferro measure --apps crm,helpdesk,gameplan,hrms,erpnext --load lazy
```

Invokes: `<binary> measure <site> [--apps ...] [--load ...]`.

### `ferro loadtest`  (aliases: `ferro bench`, `ferro matrix`)

Drive a write/read load and report peak memory + throughput. Same runtime restriction as `measure`
(no pure `ferro`).

| Flag | Default | Meaning |
| --- | --- | --- |
| `--site <name>` | forge `default_site` | Site. |
| `--runtime <r>` | forge `runtime` | `ferrod` or `native`. |
| `--apps <csv>` | — | (ferrod only) forwarded as `--apps`. |
| `--load <mode>` | `lazy` | (ferrod only) forwarded as `--load`. |
| `--threads <int>` | `4` | Forwarded as `--threads`. |
| `--rounds <int>` | `8` | Forwarded as `--rounds` (only if set; default 8). |

`bench` and `matrix` are argparse aliases that run the exact same code (`cmd_loadtest`) with the
same defaults.

```sh
ferro loadtest --threads 4 --rounds 8
ferro bench --runtime ferrod --apps crm --load lazy
```

Invokes: `<binary> loadtest <site> --threads N [--rounds R] [--apps ...] [--load ...]`.

### `ferro dev <app>`

Run an app's frontend dev server (vite), proxied to the ferro backend.

| Positional / flag | Default | Meaning |
| --- | --- | --- |
| `app` (positional) | (required) | App whose frontend to run. |
| `--port <int>` | forge `webserver_port`, else `8000` | Backend port to proxy to. |

Finds the app's frontend dir (`frontend/`, `desk/`, or `dashboard/` containing a `package.json`),
writes `webserver_port` into `sites/common_site_config.json` so the vite proxy targets your backend,
`yarn install` (or `npm install`) if `node_modules` is missing, then `execvp`s the dev server.
Dies if no frontend dir is found or neither yarn nor npm is present.

```sh
ferro dev crm
```

Invokes: `yarn dev` (or `npm run dev` if only npm is present), in the app's frontend dir.

> The CLI warns: the SPA calls Frappe `/api/method/*` surfaces ferro does not implement; the
> `/api/resource` CRUD surface works. See `docs/limitations.md`.

### `ferro frontend [app]`

Zero-node fallback: a single-file Python HTTP gateway that static-serves a built frontend and
reverse-proxies the API to ferro. No node required.

| Positional / flag | Default | Meaning |
| --- | --- | --- |
| `app` (positional, optional) | — | App whose built assets to serve. |
| `--static <dir>` | app's `dist/` or `<app>/public` | Static dir to serve. |
| `--port <int>` | `8080` | Gateway listen port. |
| `--backend-port <int>` | `8000` | ferro backend port to proxy to. |

Requests under `/api`, `/files`, `/private`, `/method`, `/assets` are proxied to
`http://127.0.0.1:<backend-port>`; everything else is served from the static root with an
`index.html` SPA fallback. With no static dir it is a pure proxy.

```sh
ferro frontend crm --port 8080 --backend-port 8000
ferro frontend --static ./crm/frontend/dist
```

Invokes: nothing external; runs an in-process `http.server` gateway.

### `ferro start`

Run every process in the forge `Procfile` (web, frontend, …) with prefixed, colourised, interleaved
output. No flags (global `--forge` applies). Lines that are blank, start with `#`, or have no `:`
are skipped. `SIGINT`/`SIGTERM` terminate all children. Dies if there is no Procfile or it has no
processes.

```sh
ferro start
```

Invokes: each Procfile command via `subprocess.Popen(cmd, shell=True, cwd=forge)`.

### `ferro verify`

Functional proof that the apps run: reads go through Rust, writes drive real controllers. Needs
`ferrod` + the apps installed. Runs a small fixed set of in-process `ferrod request` calls and
prints PASS/FAIL; exits non-zero if anything fails.

| Flag | Default | Meaning |
| --- | --- | --- |
| `--site <name>` | forge `default_site` | Site. |
| `--apps <csv>` | `crm,helpdesk,gameplan,hrms,erpnext` | Apps to load for the checks. |

The checks (hard-coded): a `GET /api/resource/CRM Deal` list returns `"data"`; a
`POST /api/resource/Retention Bonus` with a past `bonus_payment_date` triggers the controller's
`417` with the message `cannot be a past date` (run with `--dev`).

```sh
ferro verify
```

Invokes: `ferrod request <site> <method> <url> ... --apps <csv> --load all --user Administrator`,
several times.

### `ferro provision-key [user]`

Provision/print an `api_key:api_secret` for a user (pure-`ferro` runtime only).

| Positional / flag | Default | Meaning |
| --- | --- | --- |
| `user` (positional, optional) | `Administrator` | User to provision. |
| `--site <name>` | forge `default_site` | Site. |

Always uses the `ferro` binary (not `ferrod`/native).

```sh
ferro provision-key Administrator
```

Invokes: `ferro provision-key <site> <user>`.

### `ferro list-apps`  (alias: `ferro apps`)

Show the app registry (from `<ferro_home>/ferro.json`) and, if inside a forge, the apps present in
`apps/`. No flags (global `--forge` applies).

```sh
ferro list-apps
```

Invokes: nothing external; reads `ferro.json` and lists `apps/`.

### `ferro version`

Show the ferro version (from `<ferro_home>/ferro.json`, default `0.1.0`) and the home path, then
which of `ferro`/`ferrod`/`ferro-native` are built.

```sh
ferro version
```

---

## Forge layout

`ferro init` creates a bench-compatible workspace. The layout (from the `cli/ferro` docstring and
the code that writes it):

```
forge/
  ferro.json                          # forge config: {forge, ferro_home, runtime, default_site, webserver_port}
  apps/<app>/                         # cloned/copied app repos (with their frontend/)
  sites/
    common_site_config.json           # {webserver_port, socketio_port, developer_mode}  (read by the vite proxy)
    <site>/
      site_config.json                # {db_type, db_name, encryption_key, installed_apps, ...}
      db/_<hex>.db                     # the SQLite site database (decompressed from the seed)
      private/backups/
      public/files/
  logs/
  config/
  Procfile                            # `web: <ferro> serve --port N`  (+ commented frontend line)
```

Key facts grounded in the code:
- A directory is a forge iff its `ferro.json` has a truthy `"forge"` key.
- `default_site` is set automatically by the first `new-site` and is used to resolve `--site` when
  omitted. If there is no default and exactly one site exists, that one is used; with multiple sites
  and no default, the CLI tells you to pass `--site`.
- The site db is the first `*.db` (sorted) under `sites/<site>/db/`.
- `common_site_config.json` carries `webserver_port` (= init `--port`), `socketio_port`
  (= port + 1), and `developer_mode: 1`. `ferro dev` rewrites `webserver_port` to the chosen
  backend port so the vite proxy hits your backend.

---

## Where the binaries are found

`runtime_dir()` resolves in this order:
1. `$FERRO_RUNTIME_DIR` if set.
2. `<ferro_home>/target/release` if it contains any of `ferro`, `ferrod`, `ferro-native`.
3. `/home/frappe/ferro/target/release` if that directory exists (on-box dev convenience).
4. Otherwise `<ferro_home>/target/release` (even if not yet built).

`ferro_home()` is `$FERRO_HOME` if set, else the parent of the directory containing `cli/ferro`.

---

## Environment variables

These are the variables `cli/ferro` actually reads (confirmed in source):

| Variable | Read by | Effect |
| --- | --- | --- |
| `FERRO_HOME` | `ferro_home()` | Override the Ferro install root (defaults to the repo containing `bin/`). |
| `FERRO_FORGE` | `find_forge()` | Default forge directory when `--forge` is not given. |
| `FERRO_RUNTIME_DIR` | `runtime_dir()` | Override where compiled binaries are looked up. |
| `FERRO_SHIM` | set by `backend_env()` | Exported to the runtime as `<ferro_home>/framework/shim` (the CLI sets it; it does not read it). |
| `FERRO_REPOS` | read in `_transpile()`; set by `backend_env()`/`install-app` | Apps directory. Read as a fallback for transpile when outside a forge; otherwise the CLI sets it to the forge's `apps/`. |
| `FERRO_CACHE_KB` | `backend_env()` | Page-cache budget passed to the runtime; default `256` if unset. |
| `FERRO_APP_MIRROR` | `cmd_get_app()` | A local mirror dir to copy apps from before falling back to git. |
| `PYO3_PYTHON` | `find_embeddable_python()` | Explicit path to the CPython used for `ferrod` (must be `--enable-shared`); also exported to cargo when building `ferrod`. |
| `FERRO_JEMALLOC` | `jemalloc_path()` | Explicit path to `libjemalloc.so.2`; otherwise common locations are probed. |
| `MALLOC_CONF` | `backend_env()` (via `setdefault`) | jemalloc tuning. The CLI sets a default of `narenas:1,dirty_decay_ms:0,muzzy_decay_ms:0,tcache:false,background_thread:true` **only when jemalloc is found and you have not already set it**. |
| `LD_PRELOAD` | `backend_env()` | The CLI prepends the jemalloc lib path when jemalloc is found. |
| `LD_LIBRARY_PATH` | `backend_env()` (Python runs) | The CLI prepends the embeddable Python's libdir when running `ferrod`. |
| `PYTHONNODEBUGRANGES` | `backend_env()` (via `setdefault`) | Set to `1` for `ferrod` runs to trim per-code-object memory. |
| `NO_COLOR` | `_c()` | Disables coloured output. |

Notes:
- `backend_env()` only adds the jemalloc / `MALLOC_CONF` / `LD_PRELOAD` bits when a jemalloc library
  is actually located. The Python-specific vars are added only on `ferrod` runs.
- `FERRO_SHIM` and `FERRO_REPOS` are primarily *set by* the CLI for the child process; only
  `FERRO_REPOS` is also *read* (in the transpile fallback path).

---

## `setup.sh` flags

[`scripts/setup.sh`](../scripts/setup.sh) installs the toolchain idempotently: a C toolchain + git +
pkg-config + sqlite + libjemalloc (via the system package manager — apt/dnf/yum/pacman/zypper/brew),
Rust via rustup (if `cargo` is absent), an embeddable CPython built `--enable-shared` via pyenv (only
if none is present), and Node.js + Yarn. It finishes by running `ferro doctor`.

| Flag | Default | Meaning |
| --- | --- | --- |
| `--yes`, `-y` | off | Non-interactive. |
| `--no-python` | python on | Skip building the embeddable CPython (ferrod won't build; pure-ferro/native still will). |
| `--no-node` | node on | Skip Node + Yarn (`ferro dev` unavailable; backend unaffected). |
| `--python-version X.Y.Z` (or `--python-version=X.Y.Z`) | `3.13.13` | CPython version pyenv builds. |

`ferro setup` forwards only `--yes`. To use `--no-python`, `--no-node`, or `--python-version`, run
the script directly: `bash scripts/setup.sh --no-node`.

## `bootstrap.sh` flags

[`scripts/bootstrap.sh`](../scripts/bootstrap.sh) goes from a clean checkout to a serve-ready forge:
optional `setup.sh` → `ferro build` → `ferro init` → `ferro new-site` → `get-app`/`install-app` per
app → `ferro populate` → prints how to serve / run a frontend / verify. If a `ferrod` build fails it
falls back to building pure `ferro`.

| Flag | Default | Meaning |
| --- | --- | --- |
| `--forge DIR` | `forge` | Forge dir to create. |
| `--site NAME` | `dev.localhost` | Site name. |
| `--apps a,b,c` | `crm,helpdesk,gameplan,hrms,erpnext` | Apps to install. |
| `--runtime ferrod\|ferro\|native` | `ferrod` | Runtime to build. |
| `--no-setup` | setup on | Skip dependency install. |
| `--no-populate` | populate on | Skip demo data. |
| `--no-build` | build on | Use existing binaries, skip `ferro build`. |

`ferro bootstrap` forwards only `--forge`, `--apps`, `--no-setup`. For `--site`, `--runtime`,
`--no-populate`, `--no-build`, run the script directly: `bash scripts/bootstrap.sh --no-populate`.

---

## Typical session

From a clean checkout to a running API with the CRM frontend:

```sh
# 1. install the toolchain (rust, embeddable python, jemalloc, node/yarn)
ferro setup --yes

# 2. confirm everything is in place
ferro doctor

# 3. build the runtime (ferrod = embedded Python, the default)
ferro build

# 4. create a workspace and enter it
ferro init myforge && cd myforge

# 5. create a site from the frappe-core seed (becomes default_site)
ferro new-site dev.localhost

# 6. fetch + install an app
ferro get-app crm
ferro install-app crm

# 7. (optional) demo data for the read paths
ferro populate --rows 3000

# 8. serve the REST API
ferro serve --apps crm --load lazy
#   -> http://127.0.0.1:8000
curl 'http://127.0.0.1:8000/api/resource/CRM%20Deal?limit_page_length=2'

# 9. in another terminal, run the CRM frontend against it (needs node)
ferro dev crm
#   ...or, with no node, serve a prebuilt frontend:
ferro frontend crm --port 8080 --backend-port 8000

# prove the apps actually run (reads in Rust, writes via real controllers)
ferro verify

# memory / throughput (ferrod or native only)
ferro measure --apps crm --load lazy
ferro loadtest --threads 4 --rounds 8
```

Or collapse steps 1–7 into one command:

```sh
ferro bootstrap --apps crm,helpdesk
cd forge
ferro serve --apps crm,helpdesk --load lazy
```
