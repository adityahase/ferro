> **UPDATE 2026-06-06 — USS/PSS re-measurement.** Every number below uses **peak RSS**
> (`/usr/bin/time -f '%M'`), which over-counts shared pages. A follow-up re-measured **USS/PSS**
> on live processes and confirmed the RSS figures (within 2–4%, verified 3 ways), with refinements:
> the bare interpreter's true private cost is **~3.4 MB USS** (the 10 MB RSS is ~70% shared
> libpython/libc), a warm worker is **~100 MB USS / ~80 MB of which is the unshared object-graph
> heap**, and the `--preload`/COW lever cuts a 4-worker box ~350→~110 MB PSS at idle (~3.2×) but
> only **~2×** under realistic load (CPython refcount writes erode COW; `gc.freeze()` doesn't help).
> The headline verdict (don't swap the runtime) is unchanged. **See `01-uss-pss-sharing.md`.**

# Python Runtime Memory Footprint Investigation — for Frappe

**Goal:** Reduce the memory footprint of the Frappe framework. As part of a broader
effort (MariaDB→SQLite, RQ→async task queue, SocketIO→async Python, forked workers→async
Python), we are evaluating whether switching the **Python runtime** could cut memory use.

**Runtimes under test:** CPython (latest), PyPy, MicroPython, RustPython.

**Host:** `aditya-cloud`, Ubuntu 26.04 LTS, x86_64, 2 vCPU, 7.6 GiB RAM, 69 GB free disk.
**Driver:** autonomous investigation, started 2026-06-06.
**System Python:** 3.14.4. **pyenv:** 2.7.1 (pre-installed at `~/.pyenv`).

---

## TL;DR

**Switching the Python runtime will not reduce Frappe's memory footprint — and three of the four
candidates can't run Frappe at all. Stay on CPython (3.14).** Verified empirically on this host.

| Runtime | Bare RSS | Runs Frappe? | Why |
|---|--:|:--:|---|
| **CPython 3.14.4** | ~10 MB | ✅ **yes** | Reference impl. Frappe 17-dev + SQLite site running in `bench-cpython314`. |
| PyPy 7.3.22 (3.11) | ~60 MB | ❌ no | Can't parse Frappe's 3.12+ syntax; `requires-python>=3.14`; `orjson` won't build. Baseline 6× CPython. |
| RustPython 0.5.0 | ~27 MB | ❌ no | No pip, no CPython C-API → `import frappe` dies on `orjson` (line 27); no `sqlite3`. Worst per-object cost. |
| MicroPython 1.28 | ~3 MB | ❌ no | Missing core stdlib (`datetime`, `logging`, `sqlite3`, `functools`, …); no pip/PyPI; no C-API. |

**Two findings carry the verdict:**
1. **Compatibility wall:** Frappe imports `orjson` (a CPython-C-API-only extension) as its first
   third-party import. It can't load on PyPy, RustPython, or MicroPython. No alternative runtime
   can even `import frappe`.
2. **The interpreter isn't the cost:** a warm Frappe worker is **~115 MB**; the interpreter
   baseline is only **~10 MB (~8%)**. The other ~100 MB is Frappe's own object graph (imports +
   meta/controller machinery). A runtime swap can't touch it.

→ The real memory levers are **fewer resident processes** (the async migration already in flight)
and **Frappe's own import/meta footprint** (pre-fork+COW, lazy imports, bounded meta cache).
See `## Verdict`. Sources in `02-sources.md`.

---

## Method

1. Install a clean build toolchain.
2. Install each runtime in isolation (pyenv where supported; prebuilt binary otherwise).
3. **Apples-to-apples interpreter baseline:** measure resident memory (RSS) for:
   - bare interpreter doing nothing (`pass`),
   - interpreter after importing a representative set of stdlib modules,
   - interpreter holding a fixed-size data structure (to compare per-object overhead).
   Each measured the same way (peak RSS of the child process) for fairness.
4. **Frappe install:** attempt a SQLite-backed Frappe site per runtime; record success/failure
   and the *reason* for any failure (this is itself a key finding).
5. **Frappe runtime footprint:** for runtimes where Frappe runs, measure RSS of the server
   process(es) at idle and the per-worker cost.

All raw numbers live in `measurements/`. Scripts live in `scripts/`.

---

## Runtime install log

| Runtime | Version | Install method | Status |
|---|---|---|---|
| CPython | 3.14.4 (system) + 3.12.13 & 3.13.13 via pyenv | pyenv / system | ✅ installed |
| PyPy | 7.3.22 (Python 3.11.15 language level) | pyenv (prebuilt binary) | ✅ installed |
| MicroPython | 1.28.0 (unix port) | pyenv (built from source) | ✅ installed |
| RustPython | 0.5.0, git HEAD (targets Python 3.14.0-alpha) | `cargo install --git` (rustc 1.96.0) | ✅ installed |

RustPython is **not** in pyenv and ships **no prebuilt Linux x86_64 binary** on its GitHub
releases (weekly snapshot tags exist but carry no assets) — it must be compiled from source
(~15 min on 2 vCPU; pulls heavy crates incl. `aws-lc-sys`, `rustls`).

**Benches built:** `benches/bench-cpython314` — CPython 3.14.4 + **Frappe develop (17.0.0-dev)** +
**SQLite** site `mysite.sqlite` (working). No bench is possible for the other three runtimes
(see compatibility section) — they cannot `import frappe`.

---

## Interpreter baseline memory

Method: peak RSS (`/usr/bin/time -f '%M'`, i.e. `ru_maxrss`, in KiB) of a child process
running each snippet. Same harness for every interpreter → directly comparable.

Final numbers (min of 3 runs; KiB→MB, 1 MB = 1024 KiB). Raw: `measurements/baseline_final.csv`.

| Runtime | bare `pass` (MB) | + stdlib imports¹ (MB) | list of 1,000,000 ints (MB) | Δ for 1M ints (MB) |
|---|--:|--:|--:|--:|
| **MicroPython 1.28.0** | **2.6** | ✗ fails² | 3.6 | **+1.0** |
| **CPython 3.14.4** | 9.7 | 20.0 | 47.9 | +38.2 |
| **CPython 3.13.13** | 10.0 | 18.8 | 48.4 | +38.4 |
| **CPython 3.12.13** | 10.7 | 19.3 | 48.9 | +38.2 |
| **RustPython 0.5.0 (git)** | 27.3 | 41.8 | **141.3** | **+114.0** |
| **PyPy 7.3.22 (3.11)** | 60.5 | 80.1 | 71.8 | **+11.3** |

¹ import set: `os,sys,json,re,collections,datetime,sqlite3,hashlib,base64,functools,itertools,logging`
² MicroPython lacks most of these modules entirely (see compatibility section) — the import test errors out.

**Reading these numbers:**
- **Fixed (baseline) cost** dominates when you run *many* processes: MicroPython ≈ 2.6 MB,
  CPython ≈ 10 MB, RustPython ≈ 27 MB, PyPy ≈ 60 MB (≈6× CPython). For a "fork lots of workers"
  model, baseline is everything — and that's exactly the model the broader effort is replacing
  with async, which *reduces process count* and so reduces the weight of baseline relative to
  per-object/heap cost.
- **Per-object cost** reorders things: 1M ints cost CPython +38 MB (28-byte boxed int objects),
  PyPy only +11 MB (its list-of-ints strategy stores raw machine ints), MicroPython +1 MB
  (tagged small ints — no heap object), and **RustPython +114 MB** — its object representation is
  extremely heavy. So PyPy's high baseline amortizes for *few, large, long-lived* processes that
  hold lots of data, whereas RustPython is the worst of both worlds (high baseline **and** high
  per-object cost).
- Across CPython 3.12 → 3.14 the baseline is flat-to-slightly-lower (10.7 → 9.7 MB bare). There is
  **no meaningful memory win from simply moving to a newer CPython**; the footprint is structural.

---

## Frappe compatibility per runtime

### MicroPython 1.28.0 — ❌ cannot run Frappe (verified empirically)

`help("modules")` on the unix port lists only an embedded-oriented module set
(`machine`, `mip`, `ffi`, `btree`, `cryptolib`, `deflate`, `asyncio`, `socket`, `ssl`,
`json`, `os`, `re`, `hashlib`, `time`, …). Direct probes show it is **missing core CPython
stdlib modules that Frappe imports pervasively**:

```
import importlib   → ImportError: no module named 'importlib'
import sqlite3      → ImportError   (no SQLite — and we need SQLite!)
import datetime     → ImportError
import logging      → ImportError
import functools    → ImportError
import itertools    → ImportError
import base64       → ImportError
```

It also has **no pip / no PyPI**: packages come from `mip` + micropython-lib (a small,
MicroPython-specific set), not the CPython wheel ecosystem Frappe's hundreds of dependencies
live in. There is no CPython C-API, so C-extension deps (lxml, cryptography, Pillow, …) cannot
load. **Conclusion: a switch to MicroPython would mean rewriting Frappe from scratch against a
different, much smaller language/stdlib — not a runtime swap.** Useful here only as the
floor-of-the-possible footprint datapoint (~3 MB).

### Frappe's own runtime constraints (confirmed from upstream + the cloned source)

These bound which interpreters are even eligible:

- **Python version — and it just moved *up*.** The web docs say v15 is `>=3.10,<3.14`, but the
  **`develop` branch we must use for SQLite is now `frappe==17.0.0-dev` with
  `requires-python = ">=3.14,<3.15"`** (observed directly from the cloned `pyproject.toml` via the
  `uv` resolver error). Consequences:
  - The CPython bench therefore uses **Python 3.14** (the host's 3.14.4). The 3.12.13/3.13.13 I
    built can't install develop — they're kept only as baseline-comparison interpreters.
  - **PyPy is excluded by the version gate**, not just by C-extensions: it implements **3.11**,
    below develop's 3.14 floor, and it cannot even parse Frappe's 3.12+ syntax (see PyPy section).
- **SQLite backend:** supported on the **`develop`** branch (not yet in v15 stable). Enabled with
  `bench new-site <site> --db-type sqlite`. Hence every bench here is initialised with
  `--frappe-branch develop`. Verified working: the site reports `frappe.db.db_type == 'sqlite'`.
- **Heavy / C-extension dependencies** (the real compatibility gate for non-CPython runtimes):
  Frappe declares **73 direct deps** including `orjson`, `cryptography`, `lxml`, `Pillow`,
  database drivers, `redis`, etc. Anything without the CPython C-API (MicroPython, RustPython) or
  with brittle C-API emulation (PyPy's `cpyext`) fails here — `orjson` is the first wall (below).

Sources: cloned `apps/frappe/pyproject.toml` (`requires-python`, deps), Frappe forum "Initial
SQLite support", `bench new-site` docs. (See `02-sources.md`.)

### Setup gotcha worth recording

`frappe-bench` **5.29.1** shells out to **`uv`** (bundled as a dependency) to build the bench
virtualenv. If `uv` is not on `PATH`, `bench init` dies with `FileNotFoundError: 'uv'` and rolls
back. Fix: ensure the Python env's `bin/` (where `uv` lands) is on `PATH` before `bench init`.
Two more: bench runs `yarn install --check-files`, which only **yarn classic (1.x)** supports —
**yarn 4 (berry)** rejects `--check-files`. And `--skip-assets` skips `bench build` but **not**
the `yarn install` step. For a Python-only memory study, JS assets are irrelevant, so a no-op
`yarn` shim lets `bench init` complete (the Python side — import/init/connect/ORM — is fully
functional without built assets).

### The universal blocker: `orjson` on line 27 — ❌ PyPy and ❌ RustPython both fail here

`frappe/__init__.py` imports the framework's dependencies immediately:

```
14  import functools
...
27  import orjson                                   # <-- first third-party import
28  from werkzeug.datastructures import Headers
```

`orjson` is a **CPython-C-API-only** JSON library implemented in Rust via PyO3. It explicitly
does **not** support PyPy, has no MicroPython build, and cannot load under RustPython (no CPython
C-API). Verified empirically — building `orjson` fails on PyPy:

```
uv pip install orjson  (PyPy 3.11)  →  maturin/PyO3 build fails, non-zero exit
import frappe          (RustPython) →  ModuleNotFoundError: No module named 'orjson'  (line 27)
```

Because this is Frappe's **first third-party import**, *no* non-CPython runtime can even
`import frappe`. Frappe declares **73 direct PyPI dependencies** (many compiled: `orjson`,
`cryptography`, `lxml`, `Pillow`, the database drivers, …); `orjson` is just the first wall.

### PyPy 7.3.22 (Python 3.11) — ❌ cannot run Frappe (verified empirically)

Three independent hard blockers, any one of which is fatal:
1. **Source syntax.** Frappe 17 uses PEP 695 `type` alias statements (Python 3.12+). PyPy is at
   the **3.11** language level and cannot even *parse* the source:
   `SyntaxError: invalid syntax` on `type ConfType = _dict[str, Any]`.
2. **Version gate.** Frappe develop declares `requires-python = ">=3.14,<3.15"`; PyPy reports
   `3.11.15`, so `uv pip install` refuses the project outright.
3. **C-extension deps.** Even setting the above aside, `orjson` (and friends) won't build/load on
   PyPy (`cpyext` doesn't cover PyO3-built CPython-only extensions).
   PyPy historically trails CPython's language level by ~1–3 years, so (1) and (2) won't clear
   soon. **And on memory specifically PyPy is the wrong direction: its bare baseline is ~60 MB
   (≈6× CPython), already larger than a *cold* CPython Frappe import — so PyPy workers would very
   likely be *bigger*, not smaller.**

### RustPython 0.5.0 (targets 3.14) — ❌ cannot run Frappe (verified empirically)

Closer to CPython than MicroPython (it has `importlib`, `datetime`, `logging`, `functools`,
`itertools`, `ssl`, `socket`, and *can* parse Frappe's PEP 695 source), but still unusable:
- **No pip** (`No module named pip`) → cannot install Frappe's 73 PyPI dependencies.
- **No CPython C-API** → `orjson`, `cryptography`, `lxml`, `Pillow`, etc. can never load;
  `import frappe` dies at line 27 on `orjson`.
- **No `_sqlite3`** → the very database backend we want is absent.
- It is an **alpha** interpreter (0.5.0) with incomplete stdlib/semantics and, per the
  measurements above, the **worst memory profile of all** (27 MB baseline, +114 MB per 1M ints).

---

## Frappe runtime footprint

Measured on the only runtime that runs it — **CPython 3.14.4 + Frappe 17.0.0-dev + SQLite**
(`benches/bench-cpython314`, site `mysite.sqlite`). Peak RSS, min of 3 runs, fresh process each.
Raw: `measurements/frappe_cpython314.csv`.

| Scenario (fresh process each) | peak RSS (MB) | Δ over bare interp. |
|---|--:|--:|
| bare env interpreter (`pass`) | 9.4 | — |
| `import frappe` | 58.8 | +49.4 |
| `frappe.init(site)` + `frappe.connect()` (SQLite) | 61.4 | +52.0 |
| + `get_meta()` ×2 (first metadata access — warms the meta/controller machinery) | 110.4 | +101.0 |
| + `get_meta()` ×20 doctypes | 114.0 | +104.6 |
| init+connect + `get_all()` (request-like read) | 110.6 | +101.2 |

**The decisive result for the memory-reduction goal:**

- A **warm Frappe worker ≈ 110–115 MB** RSS. The **interpreter baseline (~9.6 MB) is only ~8%**
  of that. The other ~100 MB is **Frappe's own Python object graph** — imported modules
  (`import frappe` alone = +49 MB across 73 deps), Werkzeug, the ORM, and especially the
  **meta/controller machinery** that warms up on the *first* `get_meta()` (the +49 MB jump from
  61 → 110 MB). That jump is essentially **fixed**: going from 2 to 20 doctype metas added only
  ~4 MB.
- **Switching the interpreter cannot reclaim that ~100 MB** — it is framework code and data, not
  interpreter overhead. The only interpreter-dependent slice is the ~10 MB baseline, and every
  alternative either **cannot run Frappe at all** (MicroPython, RustPython) or has a **larger**
  baseline (PyPy ~60 MB).
- Newer CPython doesn't help either: 3.12 → 3.14 baseline is flat (~10 MB).

---

## Verdict

**Do not switch the Python runtime to reduce Frappe's memory footprint. It cannot work, and even
where it could it would not help.**

1. **Only CPython can run Frappe — verified, not assumed.** Frappe imports `orjson`
   (CPython-C-API-only) on line 27 of `frappe/__init__.py`, before anything else. That single
   import is unbuildable/unloadable on **PyPy**, **RustPython**, and **MicroPython**. Add PyPy's
   inability to parse Frappe's 3.12+ syntax and develop's `requires-python >=3.14` gate, and the
   non-CPython options aren't "harder" — they're **impossible** without rewriting Frappe and
   dropping its compiled dependencies.
2. **The interpreter isn't where the memory is.** A warm worker is ~115 MB; ~107 MB of that is
   framework objects, ~10 MB is the interpreter. The biggest single chunk is the meta/controller
   subsystem, which is fixed overhead per worker. No runtime swap touches this.
3. **The alternatives that *are* smaller can't run Python apps at all.** MicroPython (~3 MB) and
   RustPython are not CPython-compatible runtimes for server software — they're an embedded
   language and an alpha reimplementation. RustPython is in fact the *heaviest* per-object.

### Where the real memory wins are (aligned with the work already in flight)

The memory-reduction lever is **process count × per-worker footprint**, and the existing roadmap
already targets the high-value side of it:

- **forked workers → async Python** — the single biggest win. N prefork workers each pay the full
  ~115 MB; collapsing to a few async event-loop processes removes most of the duplication. *This
  matters far more than any interpreter choice.*
- **RQ → async task queue** and **SocketIO → async Python** — same effect: fewer always-resident
  Python processes each carrying the ~100 MB framework graph.
- **MariaDB → SQLite** — removes the MariaDB server RSS (hundreds of MB) from the box; on the
  Python side it also drops the need for the `mysqlclient`/connection-pool processes.
- **Additional Python-side levers worth measuring next** (all reduce the ~100 MB framework graph,
  which is the actual target — not the runtime):
  - **Pre-fork + copy-on-write**: import Frappe and warm the meta cache *once* in a parent, then
    fork — shared pages mean the ~100 MB is paid once, not per worker. (gunicorn `--preload`.)
  - **Lazy / trimmed imports**: `import frappe` pulls +49 MB before a request is served; auditing
    eager imports and the always-loaded doctype set would shrink every worker.
  - **Bounded meta cache**: the first `get_meta()` adds ~49 MB; cap/evict it per worker.

**Bottom line:** keep **CPython** (track 3.14, since Frappe develop now requires it). Spend the
memory budget on cutting process count (the async migration) and on Frappe's own import/meta
footprint — not on the interpreter.
