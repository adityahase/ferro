# Frappe worker memory — granular accounting (CPython 3.14, *actual usage*)

**Goal:** account for *every* chunk of a Frappe web-worker's resident memory under
**actual request-serving usage** (not idle boot), down to the MB, and identify
concrete levers to reduce the footprint.

**Status:** LIVE DOCUMENT — updated as measurements land. Started 2026-06-06.

**Host:** Ubuntu 26.04, x86_64, 2 vCPU, 7.6 GiB RAM.
**Runtime:** CPython **3.14.4** (system build; `WITH_PYMALLOC=1`, `WITH_MIMALLOC=1`
compiled in but **pymalloc is the active allocator**, `PYTHONMALLOC` unset, GIL build).
**App:** Frappe **17.0.0-dev** + **SQLite** site `mysite.sqlite` (278 doctypes, 2 users),
bench at `/home/frappe/benches/bench-cpython314`. Redis (cache+queue) running on
:13000/:11000.

> ⚠️ **The user's "256k chunk" model is one CPython generation old.** On this 3.14
> build the obmalloc **arena size is 1 MiB** (`1048576`), not 256 KiB; pools are 16 KiB.
> (The 256 KiB figure survives only as the size of the obmalloc *arena-map root* table.)
> We therefore account at three granularities: **VMA** (smaps), **1 MiB arena** (obmalloc),
> and **16 KiB pool / type-level census**.

## Workload = "actual usage" (the thing we measure)

`scripts/workload.py` drives, for 3 rounds (steady state), what a gunicorn web
worker does while serving traffic — verified all-200:
- `frappe.init` + `connect` (SQLite) + `set_user(Administrator)`
- meta/controller warmup over 44 core doctypes
- ORM reads (get_all/get_list/get_value/get_count/get_doc+as_dict) over 22 doctypes
- ORM writes (insert/update/delete ToDos in transactions; commit + rollback)
- Jinja templating + value formatting
- **real WSGI requests through `frappe.app.application`** (werkzeug test client):
  guest `frappe.ping`; api-key-auth `get_logged_user`, `client.get_list`,
  `client.get_count`, `/api/resource/*`, `client.get`.

This loads **1730 modules** (full workload). For reference: this study's `import frappe`
+ `import frappe.app` baseline is **1620** modules; the *prior* study's `import frappe`
*alone* (idle, no `frappe.app`) was 1288. So actual request-serving usage (1730) > eager
import of the web app (1620) > bare framework import (1288). **Measuring idle boot undercounts.**

---

## HEADLINE: where every MB of a warm worker lives (RSS 154.4 MB)

Single PID, after the workload, read from its own `/proc/self/smaps` +
`sys._debugmallocstats()` + glibc `mallinfo2()`. Cross-checked three ways; residual < 1 MB.

| Physical bucket | RSS | What is actually in it |
|---|--:|---|
| **obmalloc arenas** (anon mmap, 1 MiB arenas, coalesced into bigger VMAs) | **65.4 MB** | All Python objects ≤512 B. 64 arenas, **97% full** (65.2 MB live blocks, 0.8 MB free). Dominated by the **imported code graph**: code objects + their constants. |
| **glibc `[heap]` (brk)** | **53.1 MB** | `mallinfo2`: **30.2 MB in-use**, **23.8 MB free chunks retained**. In-use = Python objects >512 B (~18 MB, via PyMem_RawMalloc) + native C-ext mallocs (~12 MB: sqlite, openssl, libxml2, re). Free = **fragmentation high-water from request churn → reclaimable**. |
| **shared libraries (.so + python binary + locale)** | **35.7 MB** | libpython/python3.14 6.0, `_rust` (cryptography) 5.5, libcrypto 4.1, pydantic_core 2.8, lxml etree 2.7, libc 1.9, nh3 1.5, libstdc++ 1.5, libsqlite3 1.4, **libmysqlclient 0.8 (loaded but unused on SQLite!)**, … COW-shared across workers. |
| **thread stacks / vdso / misc** | ~0.2 MB | one resident stack page-set. |
| **TOTAL** | **154.4 MB** | (65.4 + 53.1 + 35.7 + 0.2 ≈ 154.4 ✓) |

USS = 144.0 MB, PSS = 146.7 MB (single process; almost all private). Anonymous
(private-dirty writable heap) = **122.1 MB** = obmalloc 65.4 + glibc-heap 53.1 + ~3.6 misc.

### The two big reduction targets are now named
1. **The imported code graph (~43 MB of the Python heap).** tracemalloc attributes
   **43.4 MB** to the single allocation site `<frozen importlib._bootstrap_external>:511`
   (`marshal.loads` of .pyc) — i.e. **code objects + their constants (strings/tuples)
   for 1730 modules.** Lever = fewer / lazier imports.
2. **Reclaimable glibc fragmentation (~22 MB, free).** `malloc_trim(0)` drops RSS
   **154.4 → 132.5 MB instantly (-21.9 MB, -14%)** with zero code change to live data.
   Lever = periodic trim / glibc tunables / jemalloc|mimalloc.

---

## ★ BOTTOM LINE — recommended stack (all measured, all compose)

Three independent levers hit three different buckets, so they **stack**:

| Lever | What it targets | Effort | Measured effect |
|---|---|---|---|
| **A. `gunicorn --preload`** | duplicated framework graph across workers | **1 flag** | 4-worker pool PSS **394 → 204 MB (−48%)** |
| **B. Lazy-import 11 eager deps** | the 43 MB import graph (per worker + shared base) | small Frappe patches | per-worker **−28 MB / −409 modules** |
| **C. jemalloc `dirty_decay_ms:0`** *or* periodic `malloc_trim` | glibc fragmentation from churn | env / preload hook | **−4 to −22 MB/worker** (churn-dependent) |

**Single warm worker:** 155 → ~107 MB (B+C). **4-worker box (the real win):** today ~394 MB
PSS (no preload) → with A+B+C an estimated **~150–175 MB PSS (≈55–60% less)**. Keep CPython
3.14 + pymalloc (disabling pymalloc is *worse*). `gc.freeze` is already on and harmless but
is **not** a lever (refcount writes, not GC, erode COW). Order of impact: **A ≫ B > C**.

The two structural costs that remain after all levers — the **~43 MB imported code graph**
(obmalloc) and the **per-worker refcount-driven COW erosion** — are CPython-architectural;
shrinking them further means fewer imports (B, ongoing) or the free-threaded/deferred-refcount
build (separate study).

## The warmup ladder (fresh process per stage; serial, no contention)

| Stage | RSS MB | USS MB | Anon MB | modules | live-objs census MB | code-obj MB |
|---|--:|--:|--:|--:|--:|--:|
| bare interpreter (`pass`) | 12.0 | 9.1 | 4.9 | 64 | 3.2 | 0.5 |
| `import frappe` + `frappe.app` | 117.9 | 108.1 | 87.7 | 1620 | 60.7 | 14.1 |
| + `init` + `connect` (SQLite) | 118.5 | 108.6 | 88.0 | 1623 | 61.2 | 14.1 |
| + `set_user` + warm 44 metas | 124.2 | 113.9 | 93.0 | 1632 | 64.7 | 14.1 |
| **+ full workload (ACTUAL USAGE)** | **154.4** | **144.0** | **122.1** | **1730** | **68.0** | **15.0** |

**Reading the ladder — the surprises that only actual usage reveals:**
- **`import frappe`(+app) is the cliff: +106 MB RSS in one step.** (The prior study's
  "+49 MB for import" measured `import frappe` *alone*; importing `frappe.app` — which a
  real worker does — pulls the whole web/dep stack and roughly doubles it.) Meta warmup
  is now minor (+5.7 MB), not the +49 MB "meta jump" the idle study saw.
- **Request handling (warmmeta → workload) adds +30.2 MB RSS but only +3.3 MB of live
  objects.** So **~27 MB of "actual usage" cost is retained transient/fragmentation**, not
  live data — exactly the glibc free-chunk pile `malloc_trim` reclaims. **Idle-boot
  measurement misses this entirely.**

---

## obmalloc arena accounting (`sys._debugmallocstats`, workload state)

```
64 arenas * 1,048,576 bytes/arena   =   67,108,864   (64.0 MiB reserved)
bytes in allocated blocks           =   65,222,464   (62.2 MiB live, 97.2% of reserved)
bytes in available blocks           =      805,408   (free inside used pools)
16 unused pools * 16384             =      262,144
lost to pool headers/quantization/alignment ≈ 0.8 MB
```
obmalloc is **not** the fragmentation problem — it's 97% packed. Its size is driven
purely by *how many small Python objects are live*, which is the import graph. The
arena-map root/mid/bot bookkeeping costs 0.66 MB (three 256 KiB/128 KiB tables).

## glibc heap (`mallinfo2`, workload state)

```
arena (brk total)   = 54.00 MB
uordblks (in use)   = 30.22 MB
fordblks (free)     = 23.78 MB     <-- retained fragmentation
hblkhd (mmap'd)     =  2.22 MB
ordblks (free #)    = 1981 free chunks
malloc_trim(0)      -> RSS 154.4 -> 132.5 MB  (reclaimed 21.9 MB; 1.9 MB free is trapped between live chunks)
```

---

## tracemalloc: Python-heap allocation attribution (current 83.7 MB / peak 84.6 MB)

(tracemalloc sees PyMem/obmalloc + PyMem_Raw; it does **not** see raw `malloc` inside
C extensions — those are in the memray-native section.)

| MB | site | meaning |
|--:|---|---|
| **43.42** | `importlib._bootstrap_external:511` (marshal.loads of .pyc) | **code objects + constants of all 1730 imported modules** — the dominant cost |
| 4.05 | `frappe/utils/redis_wrapper.py:102` | Frappe redis client-cache structures (n=24,867) |
| 2.40 | `importlib._bootstrap:491` | module object/dict setup |
| 1.23 | `abc:106` | ABC subclass registries |
| 0.63 | `logging:1248` | log file buffers |
| 0.41 | `num2words/lang_EU.py` | eager number-words language tables |
| 0.40 | `pypdf/_codecs/adobe_glyphs.py` | eager Adobe glyph table |
| … | re/_compiler, enum, dataclasses, typing, jinja2, cssutils, whoosh, charset_normalizer | stdlib + dep static data |

**Python heap by package:** importlib(code) ~47, **frappe 6.4**, num2words 1.8, redis 1.5,
whoosh 1.4, encodings 1.4, chardet 1.2, pypdf 1.2, werkzeug 1.1, typing 1.0, enum 0.9,
pydantic 0.8, logging 0.8, dataclasses 0.7, pypika 0.7, jinja2 0.7, cssutils 0.7, bs4 0.6, …

## Live-object census by type (full heap walk incl. atomics, getsizeof; total 68.0 MB)

| type | MB | count | type | MB | count |
|---|--:|--:|---|--:|--:|
| code | 15.0 | 39,657 | tuple | 2.7 | 38,291 |
| str | 13.7 | 173,838 | int | 1.7 | 48,117 |
| dict | 13.1 | 19,717 | list | 0.8 | 8,325 |
| function | 6.7 | 41,576 | frozenset | 0.7 | 1,624 |
| type | 6.3 | 4,850 | set | 0.7 | 1,377 |

39,657 code objects + 173,838 strings + 41,576 functions + 4,850 classes — all a direct
function of the **1730-module import graph**.

## Dependency bloat — modules pulled by `import frappe` (count) and their static cost

| package | #modules | notes / lazy-import candidate? |
|---|--:|---|
| frappe | 314 | own code (unavoidable; but auto-loads many controllers) |
| encodings | 97 | stdlib codecs |
| requests | 92 | HTTP client |
| **num2words** | **62** | number→words, ~30 langs of tables; rarely per-request → **LAZY** |
| **whoosh** | **59** | pure-python full-text + morphology tables → **LAZY** |
| **oauthlib** | **58** | OAuth only → **LAZY** |
| **pypdf** | **48** | PDF only → **LAZY** |
| **chardet** | **43** | + charset_normalizer; encoding detection → **LAZY** |
| pydantic | 42 | who pulls it? audit |
| **cssutils** | **40** | CSS (email inlining) → **LAZY** |
| cryptography | 34 | needed (auth/crypto) |
| **rq** | **26** | task queue — a *web* worker shouldn't import it → **LAZY** |
| sqlparse, xml, jinja2, urllib3 | 20-28 | mixed |

---

## ★ VALIDATED LEVERS & STACKING (warm steady-state, 3× each, σ < 0.3 MB)

Same full workload; **warm** redis (cold-cache first run is ~166 MB — see note). Two
non-invasive levers stack to cut a worker **155 → 107 MB (−31%)**:

| Config | RSS MB | after `malloc_trim` | Anon/private MB | modules |
|---|--:|--:|--:|--:|
| **glibc + eager imports (TODAY)** | **154.9** | 132.8 | 122.4 | 1729 |
| glibc + lazy imports | 127.2 | **105.0** | 97.7 | 1320 |
| jemalloc `dirty_decay_ms:0` + eager | 135.4 | 135.4 | 102.3 | 1729 |
| **jemalloc `dirty_decay_ms:0` + lazy** | **107.4** | 107.4 | 77.3 | 1320 |

- **Anon/private** (the part that does **not** COW-share across forked workers, so it
  multiplies by worker count) drops **122 → 77 MB**. With 5 gunicorn workers that's
  ~225 MB of private RAM saved on one box, before pre-fork COW even enters.

### Lever 1 — Lazy-import 11 feature-only deps: **−27.7 MB, −409 modules (measured)**
Defer these (all eagerly imported today; all verified non-essential to the basic API
serve path — requests still return 200 when stubbed): **num2words, babel, whoosh,
oauthlib, posthog(→requests/urllib3), pypdf, cssutils, bs4(→html5lib), rq(→redis dup),
pymysql**. Exact eager-import sites to fix (from `import_tracer.py`):

| dep | eager import site (edit here to defer) | feature it serves |
|---|---|---|
| num2words | `frappe/app.py:44` | number→words |
| babel | `frappe/app.py:41` | locale formatting |
| whoosh | `frappe/search/website_search.py:7` | website full-text search |
| cssutils, bs4, pypdf | `frappe/utils/pdf.py:11,17,19` | **PDF/print stack (−16 MB alone)** |
| PIL | `frappe/core/doctype/file/file.py:13` | image processing |
| oauthlib | `frappe/integrations/oauth2.py:6` | OAuth |
| pydantic | `frappe/utils/typing_validations.py:9` | ⚠ used on get_list path — NOT free to defer |
| **pymysql** | `frappe/database/mariadb/schema.py:1` | **MariaDB driver — loaded on SQLite!** |
| rq (→redis,cryptography) | `frappe/monitor.py:10` | task-queue monitor (web worker doesn't need) |
| requests | `posthog/request.py:8` | **telemetry** pulls full HTTP stack |
| croniter | `frappe/utils/scheduler.py:17` | scheduler |

Single-lib wins on the serve path: pypdf −5.5, whoosh −3.6, num2words −3.3, cssutils −2.4,
rq −2.0, pymysql −1.1, oauthlib/posthog/babel ~−0.7 each; PDF-stack together −16, all 11 → −28.
(chardet/charset_normalizer **are** used on the live request path — cannot defer; ~3+3 MB.)

#### Verified implementation plan (source-audited; ranked easiest → hardest)
A second investigation independently read the Frappe source at each eager-import site:

| # | file / dep | defer? | concrete change | risk |
|---|---|---|---|---|
| 1 | `utils/scheduler.py` — croniter | **YES** | move `from croniter import …` into `enqueue_events()` | negligible (off request path) |
| 2 | `search/website_search.py` — whoosh | **YES** | move `from whoosh.fields import …` into `get_schema()` | very low |
| 3 | `monitor.py` — rq (→redis,cryptography) | **YES** | move `import rq` into `collect_job_meta()` | low |
| 4 | `integrations/oauth2.py` — oauthlib | **YES** | move imports into `get_oauth_server()` + handlers | minimal |
| 5 | `app.py` — babel, num2words | **YES** | **just delete** lines 41–44 — `utils/data.py` *already* lazy-imports them; the eager copy is redundant pre-load (and `gc.freeze` doesn't pay off — see lever 3) | minimal |
| 6 | `database/mariadb/schema.py` — pymysql | **YES** | move `from pymysql.constants.ER import DUP_ENTRY` into `alter()`; ideally don't import the mariadb backend at all on SQLite | low (db-type gated) |
| 7 | `utils/pdf.py` — cssutils, pdfkit, bs4, pypdf | **RISKY** | has module-level side effects (`pdfkit.source.unicode=str`, `cssutils.log.setLog`, type hints, `FrappePDFKit` subclass). Needs `from __future__ import annotations` + a cached `_init_pdf_utils()`. **Biggest single win (−16 MB)** — worth the refactor. | high (test PDF/print) |
| 8 | `core/doctype/file/file.py` — PIL | **NO** | `ImageFile.LOAD_TRUNCATED_IMAGES=True` must run at import | blocking |

7 of 8 are deferrable; ranks 1–6 are low-risk and sum to most of the −28 MB; the PDF stack (#7) is the largest single chunk and needs a small refactor.

### Lever 2 — Allocator: jemalloc `dirty_decay_ms:0` **−19.5 MB automatic**, or `malloc_trim` **−22 MB on demand**
- `LD_PRELOAD=libjemalloc.so.2 MALLOC_CONF=background_thread:true,dirty_decay_ms:0,muzzy_decay_ms:0`
  → 154.9 → **135.4 MB**, continuously (no app change, no explicit trim).
- glibc + periodic `malloc_trim(0)` → reclaims **~22 MB** of retained free chunks on demand
  (154.9 → 132.8). glibc env tunables alone do **not** auto-release (single-threaded;
  free chunks aren't at heap top until trimmed).
- **Keep pymalloc**: `PYTHONMALLOC=malloc` is *worse* (158.9). tcmalloc (162) and default
  mimalloc (159) are worse; only jemalloc-decay0 (135) and mimalloc-purge0 (141) beat glibc.

### Lever 3 — Pre-fork + COW (`gunicorn --preload`): **−190 MB PSS / 4-worker pool (MEASURED, biggest lever)**
Gunicorn does **not** preload by default → each worker imports the whole app independently
(zero sharing). With `--preload` the master imports once and workers fork → COW-share the
.so + code + framework object graph. Measured on the real 4-worker pool (light traffic):

| 4-worker pool | total RSS | **total PSS (real RAM)** | total Private_Dirty | per-worker PSS |
|---|--:|--:|--:|--:|
| **no `--preload` (TODAY'S DEFAULT)** | 509 | **393.7** | 365.1 | 78.7 |
| **`--preload`** (gc.freeze on) | 538 | **204.4** | 111.5 | 40.9 |
| `--preload`, `FRAPPE_TUNE_GC=0` (gc.freeze off) | 538 | 201.2 | 107.5 | 40.2 |

`--preload` alone cuts the pool's real RAM (**PSS**) nearly **in half**; the more workers,
the bigger the win (the framework graph is paid once, not N times). **This is the single
largest lever measured.** Stacks with lazy imports (shrinks the shared base) and jemalloc.

#### ⚠ `gc.freeze` gives ~0 benefit — the COW killer is **refcounting**, not GC
Frappe already registers `gc.freeze()` + `re.purge()` as before-fork hooks
(`frappe/_optimizations.py`, gated by `FRAPPE_TUNE_GC`, default on) to protect COW. But
measured ON vs OFF is **identical** even after **1,600 requests** (PSS 214.8 vs 212.6;
Private_Dirty grows 18→120 MB either way). Reason: under load each worker dirties shared
pages mainly via **CPython refcount writes** (`ob_refcnt` lives in every object header;
touching a shared object — even read-only — flips that field and triggers a COW copy).
`gc.freeze` only avoids GC_Head writes, not refcount writes, so it can't stop the erosion.
(3.14's immortal objects, PEP 683, already spare small ints / None / interned strings /
type objects — those don't erode COW — but ordinary dicts/instances/strings still churn.)
**Takeaway:** keep `--preload` (huge); `gc.freeze` is harmless but not a memory lever on
the GIL build. A true fix needs deferred/biased refcounting (free-threaded build, separate study).

### Lever 4 — Reduce request-churn allocation
memray: `pytz/__init__.py:108` allocated **78 MB cumulative** (reading tz files; should
cache), and `frappe/database/sqlite/database.py:117` made **380,868 allocations** (per-
connection converter/function re-registration + PRAGMAs). Churn drives the reclaimable
high-water (lever 2) and CPU. Secondary to resident memory but worth Frappe's attention.

---

## Reconciliation (warm worker, glibc, RSS 154.9 MB) — every chunk accounted

```
TOP-DOWN (smaps VMA)              BOTTOM-UP (allocators + census)
  obmalloc arenas (anon)  65.4     obmalloc live blocks      65.2  (97% full, 64×1MiB)
  glibc [heap] brk        53.1       └ in-use 30.2 = py-large ~18 + native-C ~12
  shared libs + binary    35.7       └ free/frag 23.8 (→ malloc_trim reclaims 21.9)
  stacks/vdso/misc         0.3     glibc mmap'd (hblkhd)      2.2
                        ───────    shared libs/.so resident  35.7
  TOTAL                  154.4     ──────────────────────────────
                                   live Python objs (census)  68.0  (code15 str14 dict13 fn7 type6 …)
                                   of which 43.4 MB = code+consts of 1730 imported modules
```
Residual after cross-check < 1 MB. The **import graph (~43 MB)** and **reclaimable
fragmentation (~22 MB)** are the two named, measured targets; levers 1 and 2 hit them.

---

## Open measurements
- [x] memray --native — peak 108 MB; 66% of high-water is module import; native C ~12 MB.
- [x] Per-dependency marginal RSS — lever 1 sized at −28 MB (measured, all-200).
- [x] Allocator matrix — jemalloc-decay0 best automatic; pymalloc beats raw malloc.
- [x] Eager-import sites traced — exact file:line table above.
- [x] **Real gunicorn worker** — validated (section below).
- [x] `redis_wrapper.py:102` 4 MB — it's `frappe.local.cache[key]=pickle.loads(val)`, the
      in-process mirror of redis values (metas etc.). Functional per-request cache; minor.
- [x] **Adversarial verification (15-agent workflow)** — all 6 numeric claims **CONFIRMED**
      (RSS split ≤0.21 MB, obmalloc 64×1 MiB @97.2%, tracemalloc 43.4 MB, census 68 MB,
      glibc heap+trim). Module-count baseline corrected (1620 this study vs 1288 prior).
      Lazy-import feasibility source-audited (table above).
- [x] Pre-fork COW measured (lever 3): `--preload` −48% pool PSS; `gc.freeze` ≈0 (refcount).
- [ ] Implement the rank-1..6 lazy imports upstream + re-measure the real `--preload` pool
      with lazy imports (expected combined ~150–175 MB PSS for 4 workers).
- [ ] Free-threaded 3.14 build (`Py_GIL_DISABLED`) — would deferred/biased refcounting stop
      the COW erosion? (separate study; this build is the GIL build.)

### Note: cold vs warm cache (only visible under actual usage)
First request-serving process on a cold redis = **166 MB / 1740 modules**; warm steady
state = **154.9 MB / 1729 modules** (reproducible). Idle-boot measurement would report
neither — it never builds the meta/controller/query objects a served request creates.

---

## Real forked gunicorn worker — validation (the actual deployment unit)

`gunicorn -w 1 frappe.app:application`, driven by real HTTP (`curl`) traffic. Confirms
the in-process test-client numbers and the anon-dominated structure on a true forked worker.

| Worker scenario | RSS | PSS | Anon (private) |
|---|--:|--:|--:|
| boot (forked, 1 ping) | 117–122 MB | — | — |
| + 120 **read** requests (ping/get_logged_user/get_list/resource), glibc | **125.0** | 115.3 | 94.5 |
| + 120 read requests, **jemalloc decay:0** | 126.4 | 115.7 | 96.1 |
| + 200 **write** requests (POST+DELETE ToDo), glibc | 131.4 (boot 117.2 → **+14.2**) | — | — |
| &nbsp;&nbsp;…then live `malloc_trim(0)` via gdb | **127.5** (reclaimed **−3.9**) | — | — |

**What the real worker teaches (refines lever 2):**
- The forked worker is **anon-dominated** (94 MB private of 125 RSS) exactly like the
  in-process measurement — the structure holds. (In-process is *higher*, 155 MB, only
  because that workload is heavier: writes + 44 metas + templating + 3 rounds.)
- **Lever 2 is churn-dependent, and smaller than the in-process figure suggested.**
  jemalloc gave **~0** on a read-only worker (little fragmentation to reclaim). Write
  churn grows RSS (+14 MB / 200 writes) and `malloc_trim` reclaims only part (~4 MB here)
  because much of the freed space isn't contiguous at the heap top. The big 22 MB reclaim
  in-process came largely from **import-time** transient churn, which a forked worker pays
  in the *master*, not the worker. **Net: budget lever 2 at ~4–22 MB depending on how
  write/churn-heavy the worker is; jemalloc-decay0 is the safest way to keep it continuously low.**
- **Lever 1 (lazy imports, −28 MB) is the workload-independent, structural win** and the
  one to prioritise.
