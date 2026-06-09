# ferro

ferro is a **drop-in Rust replacement for the CPython + Frappe web worker**. It serves the
**same Frappe v1 REST API** against the **same** SQLite site a Frappe bench already uses — no Python
interpreter, no Frappe import graph, no per-worker object graph — in **under 64 MB**: idle ~4.6 MB,
~17.7 MB peak under heavy concurrent load at the default thread count, ≈85% lighter than a ~115 MB
warm Frappe worker. The release binary is ~2.2 MB with three dependencies (`rusqlite`, `tiny_http`,
`serde_json`); auth, permissions, and naming are reimplemented from scratch (dependency-free Fernet).

A client (frappe-js-sdk, FrappeClient, curl) talks to ferro exactly as it would to a Frappe gunicorn
worker for CRUD + auth.

## Run it

```bash
cargo build --release --bin ferro
target/release/ferro serve /path/to/site --port 8000
# serve the Frappe Desk admin SPA too — still pure Rust, no Python:
target/release/ferro serve /path/to/site --port 8000 --desk
```

Other flags: `--threads N`, `--default-user U`, `--meta-cap N`, `--dev`. Also `ferro request <site>
<METHOD> <url> [body]` (in-process, for tests) and `ferro provision-key <site> <user>`.

## Deploy it in place of Python

ferro is built to swap into an existing Frappe bench with **one reversible change**. The source of
truth is a single flag — `web_runtime` in `sites/common_site_config.json` (`gunicorn` → `ferro`) —
read at startup by [`src/main.rs:load_web_runtime`](src/main.rs).

**Dev bench** — one command, run from inside the bench:

```bash
contrib/bench-ferro-switch.sh on      # web_runtime → ferro;  `off` reverts byte-for-byte
```

It flips the flag and rewrites the Procfile `web:` line to `ferro serve --bench-mode`. Because
`--bench-mode` hosts realtime (socket.io), background jobs, the scheduler, and the cache
**in-process**, it also drops the `socketio`/`worker`/`schedule`/`redis_*` Procfile lines — the
backend collapses to one process. `sites/`, `assets/`, `watch`, and nginx are untouched. Then restart
`bench start`. `off` restores the original Procfile and config byte-for-byte (and the bench-command
shim it installed). `status` shows the current runtime.

**Production bench** (supervisor/systemd) — `contrib/bench-ferro-switch.sh prod` prints the
`{% if web_runtime == 'ferro' %}` program-set patch; paste it into your supervisor template, set the
flag, and run `bench setup supervisor && bench restart`. nginx is untouched.

> **How close is this to a true single-flag swap?** The flag is real, is the single source of truth,
> and is read everywhere it should be. **Dev is effectively one command + one restart, fully
> reversible.** Production is *not yet* a pure one-flag flip: the supervisor program-set change is
> printed as a template rather than applied by `bench setup`. Closing that gap — a templated
> `web_runtime` branch baked into the supervisor config — is the one remaining piece of work. The
> full gap analysis is in [docs/investigations/drop-in](docs/investigations/drop-in/00-REPORT.md).

### Which binary?

Pure **`ferro`** serves REST + Desk + native CRUD with **no interpreter** — the lightest, primary
path. A bench that runs **app controllers** (`validate`/`on_submit` hooks, whitelisted
`/api/method/<app>.*`) needs **`ferrod`**: the same server built `--features python`, which boots one
embedded CPython and routes those methods into the real app code when `web_runtime=ferrod`. Build it
and point `FERRO_BIN` at it before switching.

## What it replaces — and what it doesn't

**Served** — the Frappe **v1** data plane: `/api/resource/...` CRUD, `/api/method/{ping,
get_logged_user}`, token/Fernet + Basic `api_key:api_secret` auth, doctype / row (`if_owner`) /
field (`permlevel`) permissions, and `naming_series` backed by the atomic `tabSeries` counter — read
directly off `tab<DocType>`, `tabSingles`, `tabSeries`, `__Auth`, `tabDocPerm`/`tabCustom DocPerm`,
`tabHas Role`.

**Out of scope, by design** — controller business logic and server scripts (use `ferrod`),
`/api/v2`, and full User Permissions / DocShare. The complete list of intentional gaps is in
**[docs/LIMITATIONS.md](docs/LIMITATIONS.md)**.

## Memory & fidelity

Measured on-host (smaps_rollup, peak under load = 278 doctype metas cached + 2,224 list/meta requests
+ 200 concurrent CRUD cycles):

| Config | idle RSS | peak RSS | peak USS |
|---|--:|--:|--:|
| 1 thread (idle) | 4.6 MB | — | 0.9 MB |
| **4 threads (default)** | ~5 MB | **17.7 MB** | **15.2 MB** |
| 8 threads | — | 28.7 MB | 26.3 MB |
| 16 threads | — | 46.2 MB | 43.7 MB |

Every configuration is under the 64 MB target; the default is ~3.6× under it and ~6.5× lighter than
the CPython worker it replaces (per-thread cost ≈2.5 MB). A multi-agent audit against Frappe
17.0.0-dev found **57 divergences** (4 critical, 11 high, 21 medium, 21 low); the critical/high set
and most mediums are fixed and verified by a 41-assertion suite (`measurements/verify.py`, all green)
plus a 25-probe adversarial test. Full numbers and the audit are in **[REPORT.md](REPORT.md)** and
**[measurements/](measurements/)**.

Why a Rust runtime rather than a lighter Python: the CPython interpreter itself is only ~3.4 MB; the
other ~100 MB of a warm worker is Frappe's own import + object graph, which no within-Python swap can
reclaim. ferro reclaims it by not having it. → [docs/investigations/](docs/investigations/README.md)

---

## Beyond the core runtime

The drop-in replacement *is* the Rust crate — `src/` + `Cargo.toml` (no `build.rs`; the only build
inputs live under `src/`). Everything else in the repo supports it, proves it works, or is built on
top of it:

- **Variants of the runtime.** `ferrod` (`--features python`) embeds CPython so writes drive the real
  Frappe controllers (~46 MB peak). `ferro-native` (`--features native`) compiles controllers
  **transpiled** Python→Rust, with zero libpython (~18.8 MB) — the +106 MB framework import cliff is
  avoided because ferro *is* the framework. The transpiler lives in [`transpile/`](transpile/) and
  emits the checked-in `src/native/generated.rs`.
- **Standing up a site.** [`cli/`](cli/) is a bench-style `ferro` CLI (init / new-site / install-app
  / serve). [`framework/`](framework/) is the Python `frappe` shim `ferrod` imports at runtime plus a
  591 KB frappe-core SQLite seed, so `new-site` needs no DB server. [`contrib/`](contrib/) is the
  runtime switch described above. *(cli/ and framework/ are load-bearing in production — see below.)*
- **Evidence it works.** [`demos/pyo3-apps/`](demos/pyo3-apps/) runs 5 real apps
  (crm/helpdesk/gameplan/hrms/erpnext) on `ferrod` under 64 MB. [`desk/`](desk/) is the Frappe Desk
  compatibility oracle for `--desk`. [`measurements/`](measurements/) is the fidelity + load harness.
- **Hosted signup (optional product).** [`deploy/signup/`](deploy/signup/) is a self-serve control
  plane that provisions ~15–25 MB Desk tenants behind Caddy — a product built *on* ferro, not the
  replacement itself. **Currently live in production.**
- **Docs & research.** [architecture](docs/architecture.md) · [memory](docs/memory.md) ·
  [cli](docs/cli.md) · [all-in-one backend](docs/all-in-one-backend.md) ·
  [comparison-with-bench](docs/comparison-with-bench.md) ·
  [investigations/](docs/investigations/README.md) — the measure → build → run-apps → deploy research
  arc the project rests on.

## License

MIT — see [LICENSE](LICENSE).
