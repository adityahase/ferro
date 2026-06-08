# Memory

This is the memory story for Ferro: why a Frappe worker is heavy, what Ferro removes, the full
measured tables, the compression levers ranked, and how to reproduce every number yourself.

Every number on this page is grounded in an on-host measurement. Sources:
[`docs/investigations/runtime-memory/00-FINDINGS.md`](investigations/runtime-memory/00-FINDINGS.md)
(the original CPython+Frappe study), `demos/pyo3-apps/RESULTS.txt` (the `ferrod` matrix), and
`REPORT.md` (pure-Ferro). No figure here is estimated unless it says so.

> A note on variance: PSS/USS vary roughly ±1 MB run-to-run because `libpython`/`libc` pages are
> shareable and the exact resident set depends on what else is on the box at sample time. RSS is
> the honest single-worker figure. The USS/PSS re-measurement of the original CPython study
> confirmed its RSS numbers to within 2–4%.

---

## TL;DR

| Runtime | What's resident | Idle RSS | Peak RSS @ 1 / 2 / 4 / 8 threads | vs 64 MB |
|---|---|--:|--:|---|
| **`ferrod`, 5 apps, lazy (recommended)** | all 5 apps available; controllers load on first touch | **26 MB** | 30 / 36 / **46** / 65 | ✅ under at **≤4 threads** |
| `ferrod`, 5 apps, eager (stress ceiling) | all 779 controllers resident at once ‡ | 50 MB | 55 / 60 / 70 / 91 | ✅ ≤2 threads; ❌ ≥4 threads |
| `ferro` (pure Rust, no Python) † | data plane only | ~5 MB | 12 / **18** / 29 / 46 | ✅ always |
| `ferro-native` (transpiled, no interpreter) | data plane + transpiled controllers | — | ~**18** @4T | ✅ always |
| a real CPython+Frappe worker (what we replaced) | interpreter + framework + apps | ~115–155 MB | — | ❌ |

† The pure-`ferro` row is measured at **2 / 4 / 8 / 16** threads (per `REPORT.md`), not
1/2/4/8 like the `ferrod` rows — so its **18 MB is the 4-thread peak** and 46 MB is the 16-thread
peak. Pure Rust scales to far higher thread counts within budget.
‡ **779** = the number of app controllers *eagerly imported into the interpreter* in this stress
configuration. A related-but-different figure, **798/799**, appears in
[limitations.md](limitations.md): that is the count of *doctype* controllers that successfully
**register** (run their own Python) after the NestedSet/WebsiteGenerator base-class fix. One counts
imports; the other counts registrations — they are not the same quantity.

The line to remember: **lazy loading at ≤4 worker threads runs the whole five-app ecosystem under
64 MB (≤46 MB peak).** Eager-all, or ≥8 threads in any mode, is the out-of-budget ceiling and is
stated as such, not hidden.

---

## Why Frappe is heavy — the import cliff

The starting point was an investigation into whether *changing the Python runtime* could shrink a
Frappe worker. The answer was no, and the reason is the whole motivation for Ferro: **the
interpreter is not where the memory is.**

Measured on CPython 3.14.4 + Frappe 17.0.0-dev + SQLite (peak RSS, min of 3 runs, fresh process
each):

| Scenario (fresh process each) | peak RSS (MB) | Δ over bare interp. |
|---|--:|--:|
| bare env interpreter (`pass`) | 9.4 | — |
| `import frappe` | 58.8 | +49.4 |
| `frappe.init(site)` + `frappe.connect()` (SQLite) | 61.4 | +52.0 |
| + `get_meta()` ×2 (first metadata access — warms the meta/controller machinery) | 110.4 | +101.0 |
| + `get_meta()` ×20 doctypes | 114.0 | +104.6 |
| init+connect + `get_all()` (request-like read) | 110.6 | +101.2 |

Reading this:

- A **warm Frappe worker ≈ 110–115 MB** RSS. The **interpreter baseline (~9.6 MB) is only ~8%**
  of that.
- `import frappe` alone is **+49 MB** across its 73 direct dependencies (Werkzeug, the ORM, and a
  long tail of compiled libraries).
- The single biggest chunk is the **+49 MB jump on the first `get_meta()`** — the meta/controller
  machinery warming up. That jump is essentially **fixed**: going from 2 to 20 doctype metas added
  only ~4 MB.
- A USS/PSS re-measurement refined the breakdown: the bare interpreter's *true private* cost is
  only **~3.4 MB USS** (the rest is shared `libpython`/`libc`), and a warm worker is **~100 MB USS,
  ~80 MB of which is the unshared object-graph heap**. That heap is framework code and data — a
  runtime swap *within Python* can never reclaim it.

This is what the README calls the **+106 MB framework import cliff**: importing the framework and
warming its metadata costs ~100+ MB before your app has served a single request, and almost none
of it is your app.

Two further findings sealed the verdict that you cannot escape this by swapping interpreters:
Frappe's *first* third-party import is `orjson` (a CPython-C-API-only extension), which won't load
on PyPy, RustPython, or MicroPython — so no alternative runtime can even `import frappe`. And the
smaller runtimes that exist (MicroPython ~3 MB) aren't CPython-compatible enough to run the apps at
all. The conclusion was: **stay on CPython, and attack the framework's own footprint, not the
interpreter.** Ferro is the structural version of that — it removes the framework.

---

## What Ferro removes

Ferro reimplements the Frappe **data plane** in Rust: routing, auth, meta, ORM, naming, crypto. The
apps `import frappe` and receive a **native-backed shim** instead of the real framework. Because
Ferro *is* the framework, the apps never trigger the +106 MB import cliff:

- No `import frappe` of the real framework → no +49 MB of dependency imports.
- No `get_meta()` meta/controller avalanche → no +49 MB warm-up jump.
- No ~80 MB unshared Python object graph per worker.

What remains resident is: the Rust binary (~1.7 MB), per-thread SQLite page cache + glibc arena,
and — on `ferrod` only — an embedded CPython interpreter plus whichever app controllers are loaded.
That's the entire memory budget, and it's why a worker drops from ~115 MB to tens of MB.

Ferro ships in three runtime shapes, all from one source tree:

- **`ferro`** (pure Rust) — the data plane only, no Python at all. Reads + pure-CRUD writes.
- **`ferrod`** (`--features python`) — `ferro` + embedded CPython (PyO3, one interpreter) that runs
  the apps' **real** controller lifecycle for doctypes that carry Python logic. Reads and pure-CRUD
  writes still run 100% in Rust (no GIL); Python is dispatched only on a needs-Python registry hit.
- **`ferro-native`** (`--features native`) — a parallel track that *transpiles* controllers to Rust
  and compiles them in, so there is no interpreter at all.

---

## Measured tables

All measurements: on-host, `smaps_rollup`. `ferrod` uses embedded CPython 3.13.13, jemalloc
`dirty_decay_ms:0,muzzy_decay_ms:0,narenas:1,tcache:false`, a 256 KB SQLite page cache per worker,
and `PYTHONNODEBUGRANGES=1`. The realistic load = 20-row × *all-columns* reads against populated
tables (Sales Invoice has 158 columns, etc.) + CRM Deal (with child rows) & ToDo writes; "peak" is
the high-water mark during that write-storm.

### Pure Ferro (Rust data plane, no Python)

From `REPORT.md`. Load there = all 278 doctype metas cached + 2,224 list/meta requests +
200 concurrent CRUD cycles.

| Config | idle RSS | peak RSS | peak USS |
|---|--:|--:|--:|
| 1 thread (idle) | 4.6 MB | — | 0.9 MB |
| 2 threads | — | 12.0 MB | 9.7 MB |
| **4 threads (default)** | ~5 MB | **17.7 MB** | **15.2 MB** |
| 8 threads | — | 28.7 MB | 26.3 MB |
| 16 threads | — | 46.2 MB | 43.7 MB |

Per-thread cost ≈ 2.5 MB (its own SQLite connection page cache, capped at 2 MiB, + glibc arena). A
bounded LRU meta cache (`--meta-cap`) keeps resident metadata flat under doctype churn. **Every
configuration is under 64 MB**; the default is ~3.6× under it and ~6.5× lighter than the CPython
worker it replaces. (The pure-ferro idle figure also appears as **7760 kB @ 4 threads** in the
`ferrod` matrix's own pure-Rust reference line, `ferro-demo/RESULTS.txt`.)

### `ferrod` — all 5 apps, idle (post-boot)

From `ferro-demo/RESULTS.txt`.

| Config | RSS | PSS | USS |
|---|--:|--:|--:|
| crm (eager) | 20.3 | 16.2 | 15.3 |
| helpdesk (eager) | 20.9 | 16.7 | 15.8 |
| gameplan (eager) | 21.7 | 17.6 | 16.7 |
| hrms (eager, pulls erpnext) | 29.7 | 25.5 | 24.6 |
| erpnext (eager) | 42.6 | 36.6 | 34.9 |
| **all 5 (eager, 779+ controllers)** | **50.4** | 44.4 | 42.7 |
| **all 5 (lazy)** | **25.6** | 21.4 | 20.4 |

Lazy idle (25.6 MB) vs eager idle (50.4 MB) is the loading lever in one row: ~24 MB of resident
controller code that lazy loading simply doesn't pay until first touch.

### `ferrod` — all 5 apps, peak under load, by threads (lazy vs eager)

From `ferro-demo/RESULTS.txt`.

| Config | peak RSS | peak PSS | peak USS | req/s |
|---|--:|--:|--:|--:|
| ALL (**lazy**, 1T) | 30.5 | 26.3 | 25.4 | 101 |
| ALL (**lazy**, 2T) | 36.0 | 31.8 | 30.8 | 105 |
| ALL (**lazy**, 4T) | **45.8** | 41.6 | 40.6 | 142 |
| ALL (**lazy**, 8T) | 65.1 | 60.9 | 59.9 | 148 |
| ALL (eager, 1T) | 55.2 | 49.1 | 47.4 | 104 |
| ALL (eager, 2T) | 60.4 | 54.3 | 52.6 | 90 |
| ALL (eager, 4T) | 70.3 | 64.2 | 62.5 | 135 |
| ALL (eager, 8T) | 90.6 | 84.6 | 82.9 | 146 |

**The budget line, stated plainly:**

- **Lazy ≤ 4 threads is under 64 MB** — peak 30.5 / 36.0 / **45.8** MB at 1 / 2 / 4 threads. This is
  the recommended deployment.
- **Lazy 8 threads (65.1 MB) is just over** — the first lazy configuration to cross the line.
- **Eager-all is the stress ceiling, not a deployment:** under at ≤2 threads (55.2 / 60.4 MB), over
  at ≥4 threads (70.3 / 90.6 MB).
- Memory scales **~+5 MB per worker thread** and ~linearly with how many controllers are eagerly
  resident. So the two out-of-budget envelopes are: (a) eagerly loading all 779 controllers at ≥4
  threads, and (b) ≥8 threads in any mode.

### `ferro-native` (transpiled, no interpreter)

A separate, parallel track (`src/native/`, `transpile/`) transpiles controller logic
directly to Rust and compiles it in — **no libpython, no interpreter**. The completed binary is
**2.09 MB** and (per its own measurements) peaks at **~18 MB @ 4 threads** — comparable to pure
`ferro`, since it adds no interpreter weight. Characterization: across 766 controllers / 3650
methods, ~57% of doctypes are pure-CRUD (already zero-code on Ferro) and ~64% of lifecycle hooks
are in a mechanically-transpilable subset (the main blocker is the `frappe.qb` DSL). It's the
comfortable path for the covered doctypes, with `ferrod` as the fallback for the long tail.

### Versus a real CPython+Frappe worker

A warm CPython+Frappe worker is **~115 MB** (and up to ~155 MB on newer CPython with actual usage,
per the granular study). Ferro replaces it:

| | warm CPython+Frappe | `ferrod` 5 apps lazy @4T | pure `ferro` @4T |
|---|--:|--:|--:|
| peak RSS | ~115–155 MB | **45.8 MB** | **17.7 MB** |
| reduction | — | ~2.5× lighter | ~6.5× lighter |

---

## Compression levers, ranked (strongest first)

1. **Ferro *is* the framework (Rust).** Removes the +106 MB framework import cliff entirely — no
   `import frappe`, no meta/controller warm-up, no ~80 MB Python object graph. This is structural,
   not tuning; it is the reason any of the other numbers fit.
2. **Lazy controller loading** (`--load lazy`). Build the needs-Python registry by AST-scanning the
   controllers (no import); import a controller only on its first hooked write. **Idle 50→26 MB;
   4-thread peak 70→46 MB.** This is the operative deployment lever.
3. **jemalloc `dirty_decay_ms:0,muzzy_decay_ms:0,narenas:1,tcache:false`.** Returns freed
   per-request pages to the OS immediately and removes per-thread arena fragmentation. Pinning to a
   single arena (`narenas:1`) is what keeps per-thread growth bounded.
4. **256 KB SQLite page cache per worker** (down from the 2 MB default) — the dominant per-thread
   cost, so shrinking it is the largest per-thread lever.
5. **`PYTHONNODEBUGRANGES=1`** — drops code-object position tables from the embedded interpreter.

Levers 3–5 are baked into the launcher; lever 2 is the `--load lazy` default; lever 1 is the
architecture.

---

## How to reproduce

The unified `ferro` CLI wraps the runtime binaries. Only `ferrod` and `ferro-native` expose
`measure`/`loadtest` (pure `ferro` has no such subcommand — use `--runtime ferrod` or
`--runtime native`).

```bash
# post-boot (idle) memory — RSS / PSS / USS:
ferro measure  --apps crm,helpdesk,gameplan,hrms,erpnext --load lazy

# peak memory + throughput under the realistic load, at a chosen thread count:
ferro loadtest --apps crm,helpdesk,gameplan,hrms,erpnext --load lazy --threads 4

# reproduce the out-of-budget ceilings for contrast:
ferro loadtest --apps crm,helpdesk,gameplan,hrms,erpnext --load eager --threads 4   # eager-all
ferro loadtest --apps crm,helpdesk,gameplan,hrms,erpnext --load lazy  --threads 8   # ≥8 threads
```

Useful flags (from the `ferro` CLI):

- `--load all | lazy | none` — controller resident set (`ferrod`).
- `--threads N` — worker threads (`loadtest` default 4); the dominant scaling axis (~+5 MB/thread).
- `--rounds N` — load rounds (`loadtest` default 8).
- `--runtime ferrod | native` — which binary to measure; `--apps` selects which apps to load.
- `ferro bench` is an alias for `ferro loadtest`.

The raw captured matrices live in `ferro-demo/RESULTS.txt` (the `ferrod` table above) and
`measurements/` (the pure-Ferro fidelity + memory harnesses). `REPORT.md` carries
the pure-Ferro table.

> Reminder on variance: re-running `measure`/`loadtest` will move PSS/USS by ~±1 MB and RSS by a
> few percent depending on what else is resident on the host. The budget verdicts (lazy ≤4T under,
> eager-all or ≥8T over) hold across that noise; a single borderline run near 64 MB is not a
> regression.
