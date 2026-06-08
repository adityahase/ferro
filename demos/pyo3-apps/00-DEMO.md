# Running the Frappe app ecosystem on ferro — a working, measured demo

**What this is.** A *runnable* demo of the five investigated Frappe apps — **crm, helpdesk,
gameplan, hrms, erpnext** — served by **ferro** (the Rust reimplementation of the Frappe data
plane) with their **real Python controllers** executing on top, with the worker's actual
PSS/RSS/USS measured against the **64 MB** target.

The prior investigation (`docs/investigations/apps-64mb/`) *argued* feasibility from import
floors. This demo **builds it for real**: ferro embeds CPython 3.13 (PyO3), loads a native
`frappe` shim + every app controller, serves CRUD in Rust, and drives the real controller
lifecycle in Python only where a doctype carries logic. Every number below was re-measured after
an adversarial audit (see *Audit & corrections*).

---

## TL;DR — the honest result

Measured with a **realistic** load: reads return 20 rows × *all* columns from populated tables
(Sales Invoice has 158 columns, etc.), writes create a CRM Deal **with child rows** + a ToDo.

| Mode | What's resident | Idle RSS | Peak RSS @ threads (1 / 2 / 4 / 8) | Verdict vs 64 MB |
|---|---|--:|--:|---|
| **Lazy (recommended)** | all 5 apps *available*; controllers load on first touch | **26 MB** | 30 / 36 / **46** / 65 | ✅ **under at ≤4 threads** (8T ≈ 65, just over) |
| Eager | all 779 controllers resident at once | 50 MB | 55 / 60 / 70 / 91 | ✅ at ≤2 threads; ❌ ≥4 threads |
| pure-Rust ferro (reference, no Python) | data plane only | 8 MB | ~18 | ✅ |
| `ferro-native` (parallel track — transpiled to Rust, no interpreter) | — | — | ~18 @4T | ✅ |
| a real CPython+Frappe worker (what we replaced) | — | ~115–155 MB | — | ❌ |

**Bottom line.** The recommended deployment — **lazy loading, all five apps installed and
serveable, 1–4 worker threads** — runs the whole ecosystem **under 64 MB** (≤46 MB peak). Memory
scales ~+5 MB per worker thread and ~linearly with how many controllers are eagerly resident, so
two envelopes are *out of budget* and stated as such: (a) eagerly resident-loading **all 779**
controllers at ≥4 threads, and (b) ≥8 threads in any mode. Lazy loading is the architecture's
prescribed lever and is what keeps a real tenant comfortably under budget; eager-all is a stress
ceiling, not a deployment.

- **The apps run** (verified in-process *and* over HTTP, `verify.sh` 8/8): reads served 100% in
  Rust; writes drive the **real** app controllers in embedded CPython; **child tables persist**;
  NestedSet/WebsiteGenerator doctypes register and run. Clean end-to-end proof: hrms
  `RetentionBonus.validate` rejects a past date with its exact message (HTTP 417), then ferro's
  Rust layer enforces mandatory fields.
- Raw numbers: `RESULTS.txt`. Functional proof: `VERIFY.txt`.

---

## How to run it

```bash
python3 build_db.py                 # materialise 5 apps' ~800 doctypes into the SQLite site (once)
python3 populate_demo_data.py 3000  # representative rows for the read-path doctypes (once)

cd /home/frappe/ferro               # build ferrod (ferro + embedded CPython)
PYO3_PYTHON=/home/frappe/.pyenv/versions/3.13.13/bin/python3 \
  cargo build --release --features python --bin ferrod

# run with the recommended low-memory config (jemalloc + small caches), via the launcher:
APPS=crm,helpdesk,gameplan,hrms,erpnext
./run-ferrod.sh measure  site --apps $APPS --load lazy
./run-ferrod.sh loadtest site --apps $APPS --load lazy --threads 4
./run-ferrod.sh serve    site --port 8099 --apps $APPS --load lazy --user Administrator
./run-ferrod.sh request  site POST "/api/resource/CRM Deal" '{"organization":"Acme","status":"Qualification"}'

bash bench.sh     # reproduce the full memory matrix -> RESULTS.txt
bash verify.sh    # reproduce the functional proof   -> VERIFY.txt
```

`site` = `demos/pyo3-apps/site`. Set `FERRO_LOG_SWALLOWED=1` to see controller methods
that abort on an un-ported internal (see honesty notes).

---

## Architecture

Frappe is heavy because of the *framework's* import graph (+106 MB measured). ferro reimplements
that data plane in Rust, so the apps don't need it — they import a **native-backed `frappe` shim**,
and ferro calls Python *only* for the doctypes that carry logic.

```
            HTTP request
                │
         ┌──────▼──────┐  reads + pure-CRUD writes (no GIL)   ┌───────────────┐
         │ ferro (Rust)│ ─────────────────────────────────────▶│ SQLite site   │
         │ route/auth/ │                                        │ tab<DocType>… │
         │ meta/orm    │                                        └───────────────┘
         └──────┬──────┘                                                ▲
                │ needs-Python write (registry hit)                     │ ferro_rt (native)
         ┌──────▼─────────────────────────────────────────┐            │
         │ embedded CPython 3.13 (PyO3, one interpreter)    │           │
         │  frappe shim → app controller .validate() … ─────┼───────────┘
         └──────────────────────────────────────────────────┘
```

| Component | File | Role |
|---|---|---|
| `ferrod` binary | `ferro/src/ferrod.rs` | ferro data plane + embedded CPython; selective dispatch; serve/measure/loadtest/request |
| native `ferro_rt` | `ferro/src/pyrt.rs` | exposes ferro's ORM/SQL/meta to Python; shares the request's SQLite connection via a thread-local (sound: the GIL serializes Python; each pure-CRUD Rust thread uses its own connection) |
| `frappe` shim | `framework/shim/frappe/**` | real `get_doc`/`db`/`qb`/`utils`/`Document`/exceptions + minimal `NestedSet`/`WebsiteGenerator` bases, backed by `ferro_rt`, with a permissive lazy fallback for un-ported internals |
| boot + dispatch | `framework/shim/ferro_boot.py` | imports controllers (eager or lazy), parses `hooks.py`, builds the needs-Python registry, drives the write lifecycle |
| schema / data | `build_db.py`, `populate_demo_data.py` | generate the 5 apps' schema + representative rows |

**Selective Python.** At boot ferro builds a registry of every `(doctype, lifecycle-event)` that
has *any* Python — a controller method or a `hooks.py` `doc_events` entry, including `'*'`
wildcards (**erpnext** registers `'*'.validate` for SLA/deletion-lock; **gameplan** registers
`'*'.on_trash` for its soft-delete mixin). Empty ⇒ the write runs entirely in Rust (no GIL).
**All reads, and all pure-CRUD writes, are 100% Rust.**

---

## Measured memory (smaps_rollup, on-host)

From `RESULTS.txt`. Embedded CPython 3.13.13, jemalloc `dirty_decay_ms:0,narenas:1`, 256 KB
SQLite page cache, `PYTHONNODEBUGRANGES=1`.

**Idle (post-boot):**

| Config | RSS | PSS | USS |
|---|--:|--:|--:|
| crm (eager) | 20.3 | 16.2 | 15.3 |
| helpdesk (eager) | 20.9 | 16.7 | 15.8 |
| gameplan (eager) | 21.7 | 17.6 | 16.7 |
| hrms (eager, pulls erpnext) | 29.7 | 25.5 | 24.6 |
| erpnext (eager) | 42.6 | 36.6 | 34.9 |
| **all 5 (eager, 779+ controllers)** | **50.4** | 44.4 | 42.7 |
| **all 5 (lazy)** | **25.6** | 21.4 | 20.4 |

**Peak under the realistic load (20 rows × all cols reads; CRM Deal+children & ToDo writes):**

| Config | peak RSS | peak PSS | peak USS |
|---|--:|--:|--:|
| all 5 **lazy**, 1 / 2 / 4 / 8 threads | 30.5 / 36.0 / **45.8** / 65.1 | 26.3 / 31.8 / 41.6 / 60.9 | 25.4 / 30.8 / 40.6 / 59.9 |
| all 5 **eager**, 1 / 2 / 4 / 8 threads | 55.2 / 60.4 / 70.3 / 90.6 | 49.1 / 54.3 / 64.2 / 84.6 | 47.4 / 52.6 / 62.5 / 82.9 |

PSS/USS vary ~±1 MB run-to-run (libpython is shareable). RSS is the honest single-worker figure.

---

## Compression levers applied (strongest first)

1. **ferro *is* the framework (Rust)** — removes the +106 MB framework import cliff entirely.
2. **Lazy controller loading** (`--load lazy`): build the needs-Python registry by AST-scanning
   the controllers (no import), import a controller only on first hooked write. Idle 50→26 MB,
   4-thread peak 70→46 MB. This is the deployment model.
3. **jemalloc `dirty_decay_ms:0,muzzy_decay_ms:0,narenas:1,tcache:false`** — returns freed
   per-request pages to the OS and removes per-thread arena fragmentation.
4. **256 KB SQLite page cache per worker** (from 2 MB) — the dominant per-thread cost.
5. **`PYTHONNODEBUGRANGES=1`** — drops code-object position tables.

All baked into `run-ferrod.sh`.

---

## A complementary track: transpiling controllers to Rust (`ferro-native`)

A **separate, parallel** effort (`transpile/`, `src/native/`)
transpiles controller logic *directly to Rust* and compiles it in — **no interpreter at all**. Its
characterization: across 766 controllers / 3650 methods, **57% of doctypes are pure-CRUD**
(already zero-code on ferro) and **64% are fully native-able**; **64% of lifecycle hooks** are in a
mechanically-transpilable subset (the main blocker is the `frappe.qb` DSL). The completed
`ferro-native` binary is **2.09 MB**, links no libpython, and (per its own measurements) peaks at
**~18 MB @ 4 threads** — the truly comfortable path for the covered doctypes, with this
embedded-Python `ferrod` as the fallback for the long tail. *That track is not part of this demo;
it is referenced for completeness.*

---

## Audit & corrections (what an adversarial review found, and what changed)

A multi-agent audit (each finding independently verified) raised 14 confirmed issues; the
substantive ones are addressed here:

- **Memory claim was thread-/workload-dependent.** The original "64 MB ceiling" was measured at 4
  threads against *empty* tables. Re-measured with representative data and up to 8 threads, the
  honest envelope is now stated: lazy ≤4 threads is under budget; eager-all and ≥8 threads are not.
- **Child-table data loss (Python path) — FIXED.** `dispatch_write` sent `get_valid_dict()` (which
  strips child tables); child rows silently vanished on hooked writes. Now sends `as_dict()`;
  verified (`verify.sh` #6: 2 contacts persist).
- **Base-class-dropped controllers — FIXED for 19 of 20.** 16 `NestedSet` + 3 `WebsiteGenerator`
  doctypes (e.g. CRM Sales Hierarchy, Territory, HD Article) didn't register because their base
  was stubbed, so their Python *silently didn't run*. Added minimal `frappe.utils.nestedset` /
  `frappe.website.website_generator` bases; **798/799 controllers now register**. (The earlier doc
  wrongly cited `CRMSalesHierarchy.validate` as a clean proof — it wasn't running. The clean proof
  is `RetentionBonus.validate`, which subclasses `Document` directly.)
- **Silent exception-swallow — now LOGGED.** `_run_event` no longer silently passes on a
  controller method that aborts on an un-ported internal; it records and (with
  `FERRO_LOG_SWALLOWED=1`) logs it. This made visible that some controllers *partially abort* —
  e.g. `CRMDeal.before_validate`'s SLA query hits the partial `qb` and raises; the controller's
  self-contained logic still runs, the framework-dependent part doesn't.

Remaining, **disclosed** limitations (not yet fixed):

- **"Apps run" boundary.** Reads + pure-CRUD writes are fully real. For needs-Python doctypes the
  controller's *own* logic runs **to the extent it doesn't reach an un-ported framework internal**
  (deep `frappe.*` helpers, the partial `qb`, heavy optional deps). Where it does, that branch
  aborts (now logged) and is treated as a no-op — framework feature subsystems belong to the
  out-of-budget worker. So app *business rules* run; framework *side-effects* are deferred.
- **`override_doctype_class`** is parsed from `hooks.py` but the override class is not yet
  instantiated/registered — overridden doctypes use the original controller.
- **Lazy registry is a conservative over-approximation.** The AST scan name-matches lifecycle
  methods defined in the controller file; it may over-count (346 vs 329 eager) and can *miss*
  lifecycle methods a controller *inherits* from a base in another file — so lazy mode may skip
  Python for a doctype whose logic lives only in an inherited method. Eager mode (MRO
  introspection) is exact.
- **submit / cancel / rename** lifecycle events never fire — v1 `/api/resource` exposes no route
  for them.
- **Dispatch ordering.** ferro runs `validate` before naming resolves `name` (Frappe names first),
  so `name`-based self-checks in `validate` see a blank name.
- **Generated app tables carry `parent`/`parentfield`/`parenttype` columns on non-child doctypes**
  (matching ferro's `STANDARD_COLUMNS`), so non-child reads expose these as `null` — a cosmetic
  divergence from Frappe, where they're child-table-only.
- Other write semantics follow `ferro/LIMITATIONS.md`.

---

## File inventory

```
demos/pyo3-apps/
  00-DEMO.md  README.md            ← this report + quickstart
  build_db.py  populate_demo_data.py  ← schema + representative rows
  shim/frappe/**  shim/ferro_boot.py ← the native-backed frappe shim + loader/dispatch
  run-ferrod.sh  bench.sh  verify.sh  ← launcher + memory matrix + functional proof
  RESULTS.txt  VERIFY.txt           ← captured outputs
  site/                              ← the demo SQLite site (frappe-core + 5 apps)
ferro/src/{ferrod.rs, pyrt.rs, lib.rs}  ← ferro + embedded CPython
```
