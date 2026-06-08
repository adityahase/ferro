# USS / PSS re-measurement + memory sharing — Frappe workers (CPython 3.14.4)

**Why this exists.** The original `00-FINDINGS.md` measured **peak RSS** (`/usr/bin/time -f '%M'`
= `ru_maxrss`). RSS bills *shared* pages (libpython, libc, the binary, and — for forked workers —
copy-on-write pages) to every process at full weight, so it can't answer "what does one more worker
actually cost the box?" or "who are we sharing with?". This follow-up re-measures with **USS** (truly
private = `Private_Clean + Private_Dirty`) and **PSS** (proportional set size) read straight from the
kernel's `/proc/<pid>/smaps_rollup` / `smaps`, on **live, blocked** processes (the old harness measured
processes that exit instantly and so could never be read from `/proc`).

Verified three ways: primary run (smaps_rollup, `scripts/driver.py` + `fork_cow.py` + `independent.py`),
an independent re-measure with **`pmap -XX`** (different tool, own harness), and a methodology audit.
All three agree within 2–4% (the verifiers measured at slightly idler moments → uniformly ~3% lower).

---

## 1. The old RSS numbers reproduce — but RSS over-counts the interpreter ~3×

| Scenario (single process) | RSS | PSS | **USS** | Shared (file-backed) |
|---|--:|--:|--:|--:|
| bare interpreter (`pass`) | **10.3 MB** | 4.6 MB | **3.4 MB** | 6.9 MB |
| `+ stdlib imports` | 20.0 MB | 11.0 MB | 9.3 MB | 10.7 MB |
| `import frappe` | 59.1 MB | 48.8 MB | 46.4 MB | 12.7 MB |
| `init + connect` (SQLite) | 61.4 MB | 51.1 MB | 48.6 MB | 12.8 MB |
| **warm worker** (`get_meta` ×10) | **113.1 MB** | 102.6 MB | **100.2 MB** | 12.9 MB |
| init+connect + `get_all` | 108.7 MB | 98.3 MB | 95.8 MB | 12.9 MB |

(KiB→MB at 1024. smaps_rollup, min/representative of repeated runs.)

- **The bare interpreter's 10 MB RSS is real and reproducible** — but ~6.9 MB of it is *shared* read-only
  library code. The interpreter's **true private footprint is ~3.4 MB USS.** RSS triple-counts it.
  → The "10 MB" the runtime-swap study used as the interpreter slice is itself ~70% shared pages; the
  interpreter is an even *smaller* real cost than the old report's "~8%" implied (~3% of a worker's USS).
- **Warm worker ≈ 113 MB RSS confirms the old ~115 MB.** But the decisive number is **USS ≈ 100 MB**:
  ~80 MB of it is the `[anon]+[heap]` Python object graph (52.7 MB anon + 27.9 MB heap), all
  `Private_Dirty` — **shared with no one** in a lone worker.

## 2. Who are we sharing with?

**Bare interpreter — ~6.9 MB shared with *every* process on the box that links the same files:**
`python3.14`/libpython (the biggest slice), `libc.so.6`, `libm.so.6`, `ld-linux`, `libexpat`, `libz`,
plus glibc `locale-archive` and `gconv-modules.cache`. OS-wide, not Frappe-specific.

**Warm worker additionally shares C-extension *code* with sibling workers** (only once ≥2 workers map
the same file — see §3): `_rust.abi3.so` (cryptography), `etree…so` (lxml), `_pydantic_core`, `nh3`,
`libcrypto`, `libsqlite3`, `_imaging`/`libtiff`/`libopenjp2` (Pillow), `orjson`, `_decimal`,
`_cffi_backend`, `_ctypes`, the CJK codec modules, …

**The ~80 MB object graph (imported module objects + the meta/controller cache + ORM machinery) is shared
with no one** in the independent-worker model. That is the entire ballgame.

## 3. Independent workers vs fork+COW — the finding that matters

Two real deployment models, **4 workers**, all alive simultaneously (aggregate PSS = real RAM on the box;
**never sum RSS — it quadruple-counts shared pages**):

| 4-worker model | agg RSS | **agg PSS (real RAM)** | agg USS | per-worker USS |
|---|--:|--:|--:|--:|
| **Independent** (each `import frappe` — current) | ~456 MB | **~350–360 MB** | ~325–336 MB | ~81–84 MB |
| **Fork, idle children** (`--preload`/COW) | ~468 MB | **~107–111 MB** | ~23–26 MB | ~0.8–1.5 MB |
| Fork + light request-like read | ~480 MB | **~127–163 MB** | ~43–60 MB | ~5–15 MB |
| Fork + sustained realistic read loop | — | **~154–273 MB** | — | ~13–42 MB |
| Fork + pathological `clear_cache` per req | — | **~270–388 MB** | — | ~42–71 MB |

**Mechanism, visible in the raw fields:**
- Going 1→4 *independent* workers, each worker's `Shared_Clean` climbs ~13→~32 MB and `Private_Clean`
  collapses ~19→~0 (the kernel re-accounts identical file-backed lib/.so pages as shared once siblings
  map them) — but `Private_Dirty` stays pinned at ~81 MB **per worker**. The object graph is duplicated
  4×, shared with no one.
- *Forking* a warm parent puts that ~80 MB heap into `Shared_Dirty` across the whole family; PSS divides
  it by the sharer count. Idle children add **<1.5 MB private each**.

**Honest savings range: ~3.2× at idle, degrading to ~2.2× under sustained realistic load, ~1.3× under
heavy load, and it can *invert* to ~0.9× (worse than independent) under pathological meta-cache churn**
— because children that rebuild meta privately pay *both* a fresh private copy *and* their PSS share of
the now-stale COW pages. Quote **~2×** for capacity planning, not 3×.

## 4. Why COW erodes — and why `gc.freeze()` doesn't save it

- CPython stores a **mutable refcount in every `PyObject` header**. Merely *reading* an object (bumping
  its refcount) writes to its page → COW-faults a private copy. So per-worker USS climbs from ~1 MB
  (cold fork) and plateaus around ~13–14 MB under warm-cache reads; heavier/allocating work pushes it to
  ~42 MB. This is intrinsic to CPython's GIL build and is the load-dependence above.
- **PEP 683 immortal objects help only a little (verified on 3.14.4):** `None`/`True`/`False`, small ints
  `-5..256`, and a few singletons (empty tuple…) carry the immortal refcount sentinel `0xC0000000` and are
  never written → their pages stay shared. **But interned strings are NOT immortal** (tested: interned
  `'def'` and `int(257)` both show a live mutable refcount of 4). So most of the object graph still erodes.
- **`gc.freeze()` before fork made no measurable difference** (idle 106.7 vs 106.7 MB; loaded 272 vs 273 MB).
  It only stops the *cyclic GC* from traversing-and-dirtying the surviving pre-fork graph; it does nothing
  about ordinary refcount writes and fresh allocations, which dominate the erosion. Useful as defense in
  depth, not a fix.

## 5. Caveats checked

- **`ru_maxrss` (old report) vs live snapshot RSS:** `%M` is *peak* RSS and is always ≥ a live snapshot
  (demonstrated: 91.9 MB peak vs 10.0 MB after a free). Fair to compare here because Frappe startup is
  near-monotonic (frees almost nothing) → they agree to ~1–2%. Treat any old `%M` number as an upper bound.
- **THP:** system is `always [madvise]`; THP-backing the heap would amplify COW erosion (a 2 MB hugepage
  copied on one refcount write). `AnonHugePages = 0` in the warm worker here, so THP did **not** distort
  these measurements — but on a box where it does, erosion would be worse.
- **USS is a point-in-time floor, not a fixed cost** — it grows as a worker touches more pages. Quote
  per-worker cost as a range (cold-fork → warmed-under-load), not a single number.

## 6. What changes vs `00-FINDINGS.md`

- **Headline conclusion is unaffected:** still **don't swap the Python runtime.** The two load-bearing
  facts stand — `orjson` (line 27, CPython-C-API-only) walls off PyPy/RustPython/MicroPython, and
  ~80–100 MB of the warm worker is Frappe's own object graph that no interpreter swap can reclaim. The
  interpreter's true private floor is just ~3.4 MB USS — confirmed *smaller*, not larger.
- **Refined:** the interpreter is ~3% of a worker by USS (not ~8% by RSS); the "~100 MB framework graph"
  is real and, crucially, **entirely unshared per worker** in the current model.
- **Quantified the #1 Python-side lever the old report only asserted:** pre-fork + COW (gunicorn
  `--preload`) cuts a 4-worker box from ~350 MB → ~110 MB PSS at idle (~3.2×). Now with the honest
  load-dependent caveat: steady-state benefit is closer to **~2×**, and CPython refcount churn (not GC)
  is the eroder, so `gc.freeze()` won't rescue it. Fewer resident processes (the async migration) remains
  the dominant win.

Raw data: `measurements/uss_pss_verified.md`. Scripts: `scripts/{target,driver,fork_cow,independent,analyze_smaps}.py`.
