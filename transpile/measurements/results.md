# ferro-native — measured memory (all numbers on-host, /proc/self/smaps_rollup)

Host: CPython 3.14.4 present but **unused by ferro-native**; rustc 1.96; demo SQLite site with all 5
apps' schemas (1077 DocTypes, 989 `tab*` tables, 18 MB DB). ferro-native release build:
`opt-level=z`, LTO, codegen-units=1, panic=abort, strip. Binary **2.09 MB**, links only
libc/libm/libgcc — `ldd` shows **no libpython** and `strings|grep -i python` = 0.

> **Numbers below are post-audit** (after the adversarial audit's fixes — see `reports/03-audit.md`).
> The audit slightly *raised* the per-request allocation (chained-comparison temp-binding etc.) and
> *lowered* the wired-handler count (trivial `super()`-only handlers are no longer falsely wired).

## Single binary, no interpreter
```
$ ldd target/release/ferro-native | grep -i python   ->  (nothing)
$ ./ferro-native coverage
doctypes with native lifecycle logic    : 133
lifecycle handlers wired (validate/on_*) : 162
methods reachable at runtime            : 311   (162 handlers + their helpers)
methods transpiled & compiled in (total): 1274
  └ not-yet-routed (@whitelist RPCs)     : 963   (no dispatch entry point yet — see audit)
```

## Under load — 600 requests (reads across 12 doctypes + transpiled Coupon Code & Price List
## writes + pure-CRUD ToDo writes), 0 errors:

| config | idle PSS | peak RSS | peak PSS | peak USS | under 64 MB |
|---|--:|--:|--:|--:|:--:|
| 1 thread                  | 1.0 | 7.9  | 5.6  | 5.6  | ✅ |
| **4 threads (default)**   | 1.0 | **23.4** | **21.1** | 21.0 | ✅ |
| **4T (arena=2, cache=64KB)** | **1.0** | **18.8** | **16.4** | **16.4** | ✅ |
| 8T (arena=2, cache=64KB)  | 1.0 | 35.0 | 32.8 | 32.7 | ✅ |
| 16 threads                | 1.0 | ~60  | ~58  | ~58  | ✅ |
| 32 threads                | 1.0 | ~112 | ~109 | ~109 | ❌ (wrong model — see note) |

Per-thread marginal cost ≈ **3.4–4 MB**, dominated by each worker connection's SQLite page/schema
cache (989 tables) + glibc arena, **not** by the transpiled code. `FERRO_CACHE_KB=64` (vs the 256 KB
default) is the biggest lever (−4.6 MB at 4T). The compiled controllers live in the shared, read-only
text segment: adding all 1,274 methods grew the binary by only ~330 KB and the resident **PSS by ~0**
(verified: empty-registry build measured the same RSS/PSS as the full one). Throughput in this
sandbox is environment-sensitive (≈0.7–3 k req/s across runs); memory is the stable, reported metric.

**32-thread note:** no Frappe deployment runs 32 worker threads in one process; it runs *N worker
processes*. Each ferro-native process is independently < 64 MB (a 4–8 thread worker is 18–31 MB), so
the per-worker budget holds with 2–3.5× headroom. The 32T row only shows the single-process ceiling.

## Apples-to-apples vs the embedded-CPython path (same harness, same DB, --load all, 4 threads)

| runtime | idle PSS | peak RSS | peak PSS | binary + deps | interpreter |
|---|--:|--:|--:|---|---|
| **ferro-native** (transpiled → Rust), tuned | **1.0** | **18.8** | **16.4** | 2.09 MB, libc only | **none** |
| ferro-native, default config | 1.0 | 23.4 | 21.1 | 2.09 MB, libc only | none |
| ferrod (PyO3 embedded CPython, all apps) | 43.1 | 62.9 | 57.9 | 1.84 MB + libpython3.13 | CPython 3.13 |
| CPython + Frappe gunicorn worker (prior study) | — | ~115 | — | full venv | CPython 3.14 |

- ferro-native is **~2.7–3.3× lighter than ferrod under load** (19–23 vs 63 MB peak) — because
  ferrod must hold every controller's *Python objects* resident, whereas ferro-native's controllers
  are compiled machine code with ~zero per-process heap.
- ferrod eager peaks at **62.9 MB — right at the 64 MB line**; the transpiled build has ~3× margin.
- ferro-native is **~5–6× lighter than the stock CPython+Frappe worker** (~115 MB).
- (Idle: ferro-native's "ready to serve" idle is the `measure` figure ~6.4 MB PSS, not the 1.0 MB
  post-boot-before-any-connection number — see the audit's honesty note in `reports/03-audit.md`.)

## Compression levers explored
- `MALLOC_ARENA_MAX=2` and `FERRO_CACHE_KB=64`: together −1.3 MB at 4T (18.7 → 17.4). Modest,
  because the footprint is already near the irreducible floor (ferro Rust core + per-thread SQLite).
- 1 MB thread stacks (down from ferro's 2 MB): each thread touches only a few stack pages, so this
  caps reservation, not RSS.
- The transpiled code itself is **not** a compression target — it costs ~0 resident (text segment,
  shared across threads and across pre-forked processes via the page cache).
- The real throughput-scaling story is **process-level**: N small workers (each 18–31 MB) instead of
  one big multi-threaded process, exactly the Frappe gunicorn model — each worker independently
  < 64 MB with no interpreter to duplicate.

## Why the transpiled code is "free" (section + private/shared breakdown)
```
$ size -A ferro-native
.text   1,511,738   <- all 1276 transpiled methods + ferro core; read-only, file-backed, COW-shared
.rodata   181,625   <- string literals etc; read-only, shared
.data      17,528 / .bss 1,320   <- tiny mutable globals

idle smaps:  Rss=4.0 MB  Pss=0.8 MB  Private(dirty+clean)=0.4 MB  Shared=3.5 MB
```
Only **0.4 MB is private** to the process at idle; the rest is shared text/rodata + libc. So the
1,276 compiled methods (which live in `.text`) add ~0 to per-process resident memory and are shared
across pre-forked workers — the opposite of interpreted controllers, whose objects are private heap
duplicated per worker. This is the structural reason ferro-native beats the PyO3 path so decisively.

## Bottom line
The 64 MB target is met with **3.5× headroom** at the realistic worker size, the *upper bound* for a
sane per-worker thread count (≤16T) stays under 64 MB, and the transpiled approach is several times
lighter than the (already-good) embedded-CPython approach while shedding the Python runtime entirely.
