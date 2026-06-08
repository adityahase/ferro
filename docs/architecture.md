# Ferro architecture

Ferro is a Rust runtime for the **Frappe data plane**, plus a way to run the Frappe **app
ecosystem** (crm, helpdesk, gameplan, hrms, erpnext) on top of it — fitting framework + apps under
**64 MB**. This document consolidates the design: the problem it solves, the three runtimes and when
each applies, the request flow, the `frappe` shim, the native `ferro_rt` module (and why sharing the
SQLite connection is sound), selective dispatch with the lazy registry, and the forge layout.

It is grounded in the runtime source (`src/`), the feasibility study
(`ferro-apps-investigation/`), and the running demo (`ferro-demo/`). For the memory numbers see
[memory.md](memory.md); for the honest boundary of what runs vs. what is deferred see
[limitations.md](limitations.md).

---

## 1. The problem — the framework import cliff

Frappe's value is its apps, but a *single* warm CPython+Frappe REST worker costs **~115–155 MB** RSS,
and almost none of that is your app. The prior runtime-memory investigation measured the cost
precisely:

| | RSS | modules |
|---|--:|--:|
| bare CPython 3.14 | 12 MB | 64 |
| `import frappe` + `frappe.app` | **118 MB** | 1620 |
| full warm worker | 154 MB | 1730 |

The jump from 12 → 118 MB is a **+106 MB cliff** the moment the framework is imported. About **43 MB**
of it is the marshalled **code-object graph** of frappe and its dependency stack (werkzeug, jinja2,
redis, pymysql, requests, num2words, whoosh, pydantic, cryptography, lxml, …). None of that is *app*
code — it is the framework's own web/db/email/PDF/metadata machinery.

The apps themselves are small. Loaded against a thin `frappe` stub (no real framework, no heavy deps):

| Loaded (thin shim) | RSS |
|---|--:|
| bare interpreter + shim | 11 MB |
| crm / gameplan / helpdesk (all controllers) | 14–16 MB |
| hrms (all 152 controllers) | 20 MB |
| **erpnext (all 514 doctype controllers)** | **38 MB** |
| erpnext — every non-test module eager (2268) | 54 MB |
| erpnext — hot working set (lazy) | ~22 MB |

**Ferro's answer:** make the apps `import frappe` and get a thin **native-backed shim** instead of
the real framework. When *Ferro is the framework* (the data plane is reimplemented in Rust), the
Python side loads only the interpreter + the shim + the controllers it actually touches — and that
fits 64 MB. Ferro serves the ~53% of doctypes that are pure CRUD, and *all* reads, with **zero
Python**, invoking the interpreter only for the doctypes that carry server-side logic.

---

## 2. The three runtimes — and when each applies

Ferro ships **one runtime source tree** (`src/`) that compiles to three binaries, trading
coverage for footprint. Choose with `ferro build <rt>` / `ferro serve --runtime <rt>` (or set
`runtime` in `ferro.json`).

| Runtime | Build | What it is | Peak RSS (4 threads) | Controller logic |
|---|---|---|--:|---|
| **`ferro`** | `ferro build ferro` (alias `rust`) | pure-Rust data plane, no Python at all | **~18 MB** | none — CRUD + auth only |
| **`ferrod`** | `ferro build ferrod` (default) | `ferro` + embedded CPython running the **real** app controllers | **~46 MB** (5 apps, lazy) | real Python, selectively |
| **`ferro-native`** | `ferro build native` (`--transpile` to regenerate) | app controllers **transpiled to Rust**, one binary, zero interpreter | **~18 MB** | transpiled subset (~64% of hooks) |

For reference, a real CPython+Frappe worker for the same site is **~115 MB**. All three Ferro
runtimes on the recommended path are under 64 MB (full matrix in [memory.md](memory.md)).

**When each applies:**

- **`ferro`** — you only need the REST/CRUD data plane (lists, get, create/update/delete, auth,
  permissions, naming, child tables) and no controller business logic. Always builds (no shared
  libpython needed); the lightest and the reference floor.
- **`ferrod`** — you need the apps' **real** controller lifecycle (`validate`, `before_save`,
  `on_update`, …) to run. This is the headline "apps on Ferro" experience. Needs a CPython built
  `--enable-shared` (provided by `ferro setup`); reads + pure-CRUD writes still run in Rust, and only
  the needs-Python tail enters the interpreter.
- **`ferro-native`** — a parallel track that **transpiles** controller logic directly to Rust and
  compiles it in, with no interpreter at all (2.09 MB binary, ~18 MB peak). It covers ~64% of
  lifecycle hooks (the main blocker is the `frappe.qb` DSL); `ferrod` is the fallback for the long
  tail. Built from `transpile/transpile.py`.

The shared data-plane modules (`auth`, `crypto`, `meta`, `naming`, `orm`, `util`) are **identical**
across binaries — `ferro` compiles them as-is, and `ferrod` reuses them and adds the embedded-Python
layer (`pyrt`) on top (`src/lib.rs`).

### Runtime source modules (the Rust data plane)

| Module | File | Role |
|---|---|---|
| routing / HTTP / envelope | `src/main.rs` | `ferro` binary: routing, the `{"data": …}` / v1-error envelope, body cap, DoS guards |
| `ferrod` driver | `src/ferrod.rs` | `ferrod` binary: same data plane + embedded CPython; the needs-Python registry, the hooked-write driver, `measure`/`serve`/`loadtest`/`request` |
| ORM | `src/orm.rs` | `get_list` / `get_doc` / `insert` / `update` / `delete` / `count` against the same `tab<DocType>` tables; `with_txn` wraps multi-statement writes in `BEGIN IMMEDIATE` |
| metadata | `src/meta.rs` | bounded LRU `DocType` metadata cache (`--meta-cap`); fields, flags, permlevels |
| auth / permissions | `src/auth.rs` | token + Fernet api-secret auth (401 on bad creds), `if_owner` row scoping, permlevel field masking, Custom DocPerm |
| naming | `src/naming.rs` | `naming_series` / `.####` / `format:` / expression autoname, backed by the atomic `tabSeries` counter |
| crypto | `src/crypto.rs` | dependency-free Fernet (AES-128-CBC + HMAC-SHA256 + base64) |
| native module | `src/pyrt.rs` | `ferro_rt` — exposes the ORM/SQL/meta to embedded Python (`ferrod` only, `#[cfg(feature = "python")]`) |
| utilities | `src/util.rs` | URL parsing, base64, random names, datetime |

---

## 3. Request flow

The same shape holds for all runtimes; only the "needs-Python" branch differs (it exists on `ferrod`,
is transpiled-in on `ferro-native`, and is absent on `ferro`).

```
            HTTP request
                │
       ┌────────▼────────┐  reads + pure-CRUD writes (no GIL)   ┌──────────────┐
       │   ferro (Rust)  │ ─────────────────────────────────────▶│ SQLite site  │
       │ route · auth ·  │                                        │ tab<DocType> │
       │ meta · orm ·    │                                        └──────────────┘
       │ naming · crypto │                                               ▲
       └────────┬────────┘   needs-Python write (registry hit)          │ ferro_rt (native)
                │                                                        │
       ┌────────▼─────────────────────────────────────────┐            │
       │ embedded CPython 3.13 (PyO3, one interpreter)      │           │
       │  frappe shim → app controller .validate() … ───────┼───────────┘
       └────────────────────────────────────────────────────┘   (ferrod only)
```

Concretely, for `route_resource` in `ferrod.rs`:

- **`GET` (list or single)** → `orm::get_list` / `orm::get_doc` in Rust. **Never touches Python.**
- **`POST` / `PUT` / `PATCH` / `DELETE`** → Ferro asks the registry whether the relevant lifecycle
  events have any Python attached for this doctype:
  - **no** → run the whole write in Rust (`orm::insert` / `update` / `delete`), no GIL — the common
    case.
  - **yes** → `run_py_write(con, doctype, op, data, user)` drives the controller lifecycle under the
    GIL, then `wrap_data` attaches any `frappe.msgprint` messages to the success envelope.

The Rust→Python event sets are fixed in `ferrod.rs`:

```rust
INSERT_EVENTS = before_validate, validate, before_save, before_insert, after_insert, on_update, on_change
UPDATE_EVENTS = before_validate, validate, before_save, on_update, on_change
DELETE_EVENTS = on_trash, after_delete
```

A Python exception raised by the controller is mapped back to an HTTP status + Frappe error envelope
by `map_py_err` (e.g. `*Validation*`/`*Mandatory*`/`ValueError` → **417**, `*Duplicate*` → **409**,
`*Permission*` → **403**, `*DoesNotExist*`/`KeyError` → **404**), matching the v1 REST error shape the
pure-Rust path already produces (`map_orm_err`).

> **Deferred lifecycle.** `/api/resource` exposes no route for submit/cancel/rename, so those events
> never fire on `ferrod`. See [limitations.md](limitations.md).

---

## 4. The `frappe` shim surface

The apps do `import frappe`. Ferro resolves that to a **native-backed shim** (`framework/shim/frappe/`,
on `sys.path` ahead of everything; the `ferro` CLI sets `$FERRO_SHIM` to it, `ferrod` falls back to a
compiled default). Crucially the shim pulls in **none** of the framework's heavy deps. Its surface is
dictated by what the apps actually call:

| Shim area | File | Backed by |
|---|---|---|
| `get_doc` / `get_all` / `get_value` / `new_doc` / `delete_doc` / `exists` / `count` | `frappe/__init__.py` | **`ferro_rt`** (Rust ORM) |
| `frappe.db.*` (`get_value`/`set_value`/`exists`/`count`/`sql`/`commit`/`rollback`/…) | `frappe/database/` | **`ferro_rt`** (Rust ORM / SQLite) |
| `frappe.qb` query builder | `frappe/query_builder/` | partial; compiles toward SQLite via `ferro_rt.sql` |
| `frappe.model.document.Document` (+ `BaseDocument`) | `frappe/model/document.py` | Rust-backed lazy proxy + the lifecycle state machine |
| `frappe.model.meta` | `frappe/model/meta.py` | **`ferro_rt.get_meta`** (`DocField` fieldtype/options/reqd/default/permlevel) |
| `_dict`, exceptions (`ValidationError`, `PermissionError`, `DoesNotExistError`, …) | `frappe/exceptions.py`, `frappe/__init__.py` | pure Python |
| `frappe.utils.*` (`flt`/`cint`/`cstr`/`getdate`/`add_days`/…) | `frappe/utils/` | pure Python (CPython-native, byte-identical numerics) |
| `NestedSet` / `WebsiteGenerator` bases | `frappe/utils/nestedset`, `frappe/website/` | minimal bases so those doctypes register and run |
| `throw` / `msgprint` / `bold`, `session` / `flags` / `local`, `whitelist` | `frappe/__init__.py` | exceptions + `ferro_rt.msgprint` + per-request context |
| un-ported internals | `frappe/_lazy.py` | a permissive **lazy fallback** so an un-ported `frappe.*` attribute doesn't crash the import |

The shim makes a `Document` a small Python object wrapping a Rust-held doc (read/written through the
proxy), not a fat per-request dict — which is what keeps per-request Python allocation tiny and the
headroom from being eaten by request churn.

---

## 5. `ferro_rt` — the native module, and why sharing the connection is sound

`ferro_rt` (`src/pyrt.rs`) is the native `#[pymodule]` the shim calls into. It is registered
in the interpreter **before** initialisation (`append_to_inittab!(ferro_rt)` in `ferrod.rs`), so
`import frappe`'s machinery can resolve it. It exposes Ferro's Rust data plane directly to embedded
Python: `get_doc`, `get_list`, `get_value`, `exists`, `count`, `insert`, `update`, `set_value`,
`delete`, `sql` (raw `frappe.db.sql` passthrough → SQLite), `get_meta`, `msgprint`, `whoami`,
`now_datetime`, `generate_hash`.

So when a real controller runs `frappe.get_doc(...)` or `self.insert()`, it executes against the
**same SQLite site** — with no Python framework underneath.

### Connection routing

A re-entrant `ferro_rt` call (the controller reading/writing during a hooked write) must use the
**same** connection Ferro is driving the write on, so the controller sees its own in-flight writes.
`pyrt.rs` does this with two thread-locals:

- `CUR_CON` — a borrowed pointer to the worker thread's `Connection`, set by the Rust write driver
  (`set_request_con` / `clear_request_con`) around a single GIL-held call.
- `OWN_CON` — an owned, lazily-opened per-thread fallback connection for Python-initiated work outside
  a request (e.g. the boot-time loader).

`with_con` prefers the request-scoped pointer, else the owned one.

### Why it is sound

The shared-connection pointer is dereferenced through `unsafe`, but it is safe for two reasons that
reinforce each other:

1. **The GIL serializes all Python.** Any `ferro_rt` call runs while this thread holds the GIL, so no
   two Python threads can touch the connection at once. The pointer is set for the duration of a
   single GIL-held call on the *same* thread and cleared before the borrow ends — no aliasing.
2. **Each pure-CRUD Rust thread owns its own connection.** The `ferro`/`ferrod` server gives every
   worker thread its own `Connection` (`open_conn` per thread in `cmd_serve`), so the no-Python fast
   path never shares a connection across threads either.

Writes are wrapped in `BEGIN IMMEDIATE` (`orm::with_txn`), and SQLite permits a single writer, so the
write boundary is well-defined regardless of which path (Rust or Python) drives it. This is why the
connection pointer can be shared without a lock: Python is serial by the GIL, and Rust gives each
thread its own connection.

---

## 6. Selective dispatch + the lazy registry

At boot, `ferro_boot.py` (in the shim) builds a **needs-Python registry**: for every
`(doctype, lifecycle-event)`, does *any* Python attach — a controller method **or** a `hooks.py`
`doc_events` entry, including `'*'` **wildcards**? (erpnext registers `'*'.validate` for SLA /
deletion-lock; gameplan registers `'*'.on_trash` for its soft-delete mixin.) Ferro reads this back
across the FFI boundary as JSON (`needs_python_registry_json`) into the Rust `Registry`
(`by_doctype: HashMap<String, HashSet<String>>` + a `wildcard` set). `Registry::needs(doctype, events)`
is the gate on every write:

- **empty** for the relevant events ⇒ the write runs **entirely in Rust** (no GIL);
- otherwise Ferro drives the lifecycle, acquiring the GIL and calling Python only at the populated
  trigger points.

The selective-Python dividend, measured by an AST classifier over all five apps: of **798 doctypes**,
**~423 (≈53%) are pure-CRUD** — Ferro serves them, and all reads, in Rust. The needs-Python tail is
concentrated in transactional doctypes (`validate`/`on_submit`/`on_cancel`).

### Eager vs. lazy loading (`--load`)

- **`--load all` (eager)** — import every controller at boot; the registry is exact (MRO
  introspection sees inherited lifecycle methods). Heavier resident set; a stress ceiling, not the
  deployment.
- **`--load lazy` (recommended)** — AST-scan the controllers to build the registry *without importing
  them*, and import a controller only on first hooked write. This is the operative memory lever
  (idle 50→26 MB; 4-thread peak 70→46 MB). Caveat: the AST scan name-matches lifecycle methods
  *defined in the controller file*, so it may over-count slightly and can **miss** a lifecycle method
  a controller only *inherits* from a base in another file. Eager is exact; lazy is the budget
  deployment. Details and the honest envelope are in [memory.md](memory.md).
- **`--load none`** — load no controllers (pure data-plane behaviour even on `ferrod`).

---

## 7. The forge layout

Ferro's CLI (`cli/ferro`) mirrors `bench`, and a **forge** (Ferro's workspace, created by
`ferro init`) is **bench-layout compatible** — so the Frappe app frontends' own tooling (the
`frappe-ui` vite proxy) works unmodified.

```
myforge/
├── ferro.json                # forge config (runtime, default_site, webserver_port)
├── apps/<app>/               # cloned app repos (with their frontend/)
├── sites/
│   ├── common_site_config.json   # { webserver_port } — read by the vite proxy
│   └── <site>/{site_config.json, db/<name>.db}
├── logs/   Procfile
```

The Ferro repository itself (distinct from a forge) is the monorepo, laid out:

```
ferro/
├── README.md  REPORT.md  LICENSE   # front door + the pure-Ferro runtime report
├── Cargo.toml  src/                # the Rust runtime — three binaries from one source tree
│                                   #   ferro · ferrod (--features python) · ferro-native (--features native)
├── measurements/                   # the memory / fidelity harnesses
├── cli/                            # the bench-style `ferro` CLI (python3 stdlib) + scripts/ + examples/
├── framework/
│   ├── shim/                       # the native-backed `frappe` shim (ferrod's Python side) + ferro_boot.py
│   ├── build_db.py                 # schema materialiser (the part of `bench migrate` ferro needs)
│   ├── populate_demo_data.py       # representative demo rows
│   └── seed/core.db.gz             # pristine frappe-core SQLite seed (278 doctypes, Administrator)
├── transpile/                      # the Python→Rust transpiler (the ferro-native track)
├── demos/                          # runnable demos (the ferrod PyO3 apps demo)
├── desk/                           # the Frappe Desk compatibility oracle + report
├── deploy/                         # deployment (the self-serve signup control plane)
└── docs/                           # architecture · memory · limitations · cli · comparison · investigations/
```

A typical bench-style flow:

```bash
ferro setup                        # install rust, python(--enable-shared), jemalloc, node/yarn
ferro build                        # compile the runtime (ferrod by default)
ferro init myforge && cd myforge   # create the workspace
ferro new-site dev.localhost       # site from the bundled frappe-core seed (no DB server needed)
ferro get-app crm                  # fetch an app (git clone or copy from a local mirror)
ferro install-app crm              # materialise its DocType schema into the site
ferro serve                        # serve the REST API
```

Reproduce the memory numbers:

```bash
ferro measure  --apps crm,helpdesk,gameplan,hrms,erpnext --load lazy            # idle RSS/PSS/USS
ferro loadtest --apps crm,helpdesk,gameplan,hrms,erpnext --load lazy --threads 4  # peak under load
ferro verify                                                                     # functional proof
```

Every command accepts `--forge <dir>` (default: discovered from cwd or `$FERRO_FORGE`) and most take
`--site <name>` and `--runtime <r>`.

---

## See also

- [memory.md](memory.md) — the measured footprint of all three runtimes and the compression levers.
- [limitations.md](limitations.md) — the honest boundary: what runs vs. what is deferred by design.
