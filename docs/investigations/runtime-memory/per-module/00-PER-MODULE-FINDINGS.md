# Per-Module Memory Footprint — Frappe on CPython 3.14

**Goal:** Attribute Frappe's runtime memory footprint to **individual Python modules / packages**, and
quantify how much of it is actually paid **per worker** in a real deployment. Follow-on to the runtime-swap
investigation (`../00-FINDINGS.md`), which established the *aggregate* numbers — `import frappe` ≈ +49 MB
(standalone), a warm worker ≈ 115 MB, the interpreter only ~8% of that — but did **not** say *which modules*
the ~100 MB of framework graph belongs to, nor how much of it is shareable across workers. This report does.

**Host:** `aditya-cloud`, Ubuntu 26.04, x86_64, 2 vCPU, 7.6 GiB RAM.
**Bench:** `/home/frappe/benches/bench-cpython314` — CPython **3.14.4** + **Frappe 17.0.0-dev** + SQLite
site `mysite.sqlite`. **Driver:** autonomous, 2026-06-06. All measurements run **serially** (concurrent
processes corrupt peak-RSS readings). Numbers cross-checked by a 5-agent adversarial audit (see
`## Caveats & audit`); all five framing corrections it raised are folded in below.

All raw numbers live in `measurements/`; scripts in `scripts/` (plus the COW scripts shared from `../scripts/`).

---

## TL;DR

The ~115 MB warm worker is **not** one or two fat modules — it is a **long tail of medium modules plus the
cost of loading 1,288 of them**. Four things matter:

1. **Code objects dominate the Python heap.** The largest single Python-heap bucket is the import machinery
   holding the **compiled code of every loaded module** — `<frozen importlib._bootstrap_external>` ≈ **34 MB**
   warm. Frappe's value isn't a few big data tables; it's *1,288 modules' worth of bytecode + baked-in
   constants*. Fewer imported modules ⇒ less memory, near-linearly.
2. **The +49 MB "meta jump" is lazy imports, not metadata.** A bare `import frappe` loads **589** modules.
   The *first* `get_meta()` jumps that to **1,288** (+699 from import / +676 from connect) and **+48.8 MB
   real RSS** — because loading DocType controllers pulls heavy libraries at module top-level (`pypdf`,
   `bs4`, `cssutils`, `chardet`, `PIL`, `lxml` …) that a typical read request never uses. The *second*
   distinct `get_meta` costs only **+0.09 MB** — proof the jump is an import avalanche, not meta objects.
3. **The biggest deployment lever is pre-fork + copy-on-write — and it is now measured.** 4 *independent*
   warm workers cost **351 MB** PSS (real box footprint), 82 MB private each. 4 workers **forked from one
   pre-warmed parent** cost **108 MB** PSS idle and **124 MB** while serving light reads — a **~3.2× / ~2.8×
   cut** — because the ~80 MB framework heap stays `Shared_Dirty` (COW-shared); refcount writes dirty only
   ~5 MB per child. **Erosion is load-dependent:** idle ~3.2× → light read ~2.8× → sustained loop ~2× →
   pathological meta-cache churn ~1× (can invert). A concurrent USS/PSS study (`../01-uss-pss-sharing.md`,
   `../measurements/uss_pss_verified.md`) reproduced these with a *separate* harness within noise — quote
   **~2×** for capacity planning, ~3× only for mostly-idle workers.
4. **Two memory traps to avoid in a worker:** never import a **REPL** (`IPython` = 55 MB standalone), and
   keep heavy **integrations lazy** (`googleapiclient` 48, `premailer` 43 *(already lazy)*, `posthog` 40,
   `sentry_sdk` 32, `faker` 28 MB) — confirmed *not* resident today; add a CI guard so they stay out.

> **+49 vs +31 — read carefully.** "+49 MB" is the *standalone* cost of a fresh process that imports **only**
> frappe (it re-pays orjson/werkzeug/stdlib). The *marginal* cost of the `import frappe` step **in-process**,
> after stdlib+orjson+werkzeug are already loaded, is **+31 MB**. Both are correct; they answer different
> questions. This report labels which is which everywhere.

The interpreter-swap conclusion stands and is reinforced: the memory is in **framework modules**, so the
levers are **share more** (pre-fork+COW, measured ~65 %), **import less** (lazy/trim), and **fewer processes**
(the async migration) — never the interpreter.

---

## Method — five lenses, each with a known blind spot

No single technique attributes memory correctly, so they are reported together. Scripts:
`scripts/measure_per_module.py` (`incremental|tracemalloc|census`), `scripts/measure_isolation.sh`, and the
COW pair `../scripts/fork_cow.py` + `../scripts/independent.py` (+ `../scripts/target.py`).

| Lens | Measures | Captures | Blind spot |
|---|---|---|---|
| **A. Incremental RSS** | one process; `VmRSS` after each import step in Frappe's startup order | **real RSS** incl. native memory; the macro dep→framework→meta split | order-dependent; deltas are post-`gc.collect()` (lower bound on steady state); two steps sample below peak (see caveats) |
| **B. Isolation RSS** | fresh process per module; peak RSS (`ru_maxrss`), min-of-3 | **standalone cost incl. native memory** | re-counts shared transitive deps ⇒ does **not** sum to `import frappe`; answers a *hypothetical* — pair with the resident-Y/N flag |
| **C. tracemalloc** | Python-heap bytes attributed to the allocating module | precise **Python-object** bytes per module | misses **all** native/C-ext memory; its own *RSS* is inflated ~2.5× — discard it (only traced-object totals are usable) |
| **D. sys.modules census** | module count + per-package code-object bytes | structural breadth; re-attributes some code to owning package | code_bytes ≈ 18 % of RSS — a *ranking* metric, not a budget; first-touch attribution is order-dependent |
| **E. COW / smaps** | PSS/USS/Shared across forked vs independent workers (`/proc/*/smaps_rollup`) | the **real per-worker footprint** in deployment | needs `gc`-quiet workers; refcount churn under load is captured via the `do_work` cell |

RSS in KiB (1 MB = 1024 KiB); tracemalloc/code in bytes (1 MiB = 1,048,576 B). The bare interpreter is
9.6 MB (isolation, clean `python -c pass`); the in-process incremental baseline is 12.5 MB (harness already
loaded) — a ~2.8 MB offset, so the two columns' **absolutes** aren't directly comparable, only deltas within.

---

## A. The macro split — incremental RSS (mean of 3 runs)

Raw: `measurements/incremental.csv`. One process, `VmRSS` after each step (`gc.collect()` first ⇒ deltas
are *retained-after-forced-GC*, a lower bound on steady state). Deltas are the signal, not the 12.5 MB
in-process baseline.

| Step | cum RSS (MB) | Δ (MB) | modules |
|---|--:|--:|--:|
| interpreter start (harness loaded) | 12.2 | — | 64 |
| + stdlib core¹ | 13.5 | +1.3 | 84 |
| + `import orjson` | 14.0 | +0.5 | 92 |
| + `from werkzeug… import Headers` | 28.4 | **+14.3** | 205 |
| + `import frappe` (marginal) | 59.5 | **+31.1** | 589 |
| + `frappe.init(site)` | 61.5 | +2.0² | 606 |
| + `frappe.connect()` (SQLite) | 61.8 | +0.3² | 612 |
| + **first** `get_meta('User')` | 110.6 | **+48.8** | **1288** |
| + `get_meta('DocType')` (2nd, distinct) | 110.7 | **+0.1** | 1288 |
| + 18 more metas | 114.5 | +3.8 | 1288 |
| + `get_all('User')` | 114.5 | +0.0 | 1288 |

¹ `os,sys,re,json,functools,importlib,inspect,threading,warnings,collections`
² init/connect *allocate-then-free*: `VmRSS` dips ~0.4 MB below the same-step `VmHWM` peak in all 3 runs
(captured in the `vmhwm_kib` column). The reported delta is post-GC resident, not the allocation peak;
magnitude is <0.5 MB.

**Reading it:** four events are nearly everything — **werkzeug +14 MB**, **rest of `import frappe` +31 MB**,
**first `get_meta` +49 MB**, **+3.8 MB** for the next 18 metas. The first `get_meta` is the largest jump and
coincides with the module count more than doubling (612→1288). The next distinct `get_meta` is **+0.09 MB** —
so the +49 MB is a one-time **controller-import avalanche**, not the cost of a metadata object.

---

## B. Per-module standalone cost — isolation RSS (min-of-3)

Raw: `measurements/isolation.csv`. Fresh interpreter importing only that module. `bare`=9.6 MB. The
Δ-over-bare column is the standalone cost. **It does NOT sum to `import frappe`** (shared transitive deps are
re-counted — summing all 63 rows = 16× the real footprint) and it answers a *hypothetical*. So every row is
paired with **resident-in-warm-worker?** (from the census). That flag separates a real cost (orjson,
werkzeug, redis, pydantic, and the meta-loaded pypdf/bs4/cssutils/chardet) from a hypothetical (IPython,
googleapiclient, premailer, posthog — *not* resident).

| module | standalone (MB) | Δ over bare (MB) | resident in warm worker? |
|---|--:|--:|:--|
| **`import frappe` (whole, standalone)** | 59.0 | 49.6 | — (the anchor; cf. +31 MB marginal in lens A) |
| `IPython` | **55.0** | 45.6 | **no** (REPL — `bench console` only) |
| `googleapiclient` | 48.2 | 38.8 | **no** (lazy) |
| `premailer` | 43.4 | 33.9 | **no** (already lazy — `email_body.py:470`, function-local) |
| `posthog` | 39.6 | 30.2 | **no** (lazy) |
| `pypdf` | 37.9 | 28.5 | **yes** ← meta-warm (top-level in `utils/pdf.py`, `print_format.py`) |
| `requests_oauthlib` | 36.0 | 26.6 | **no** |
| `markdownify` | 35.7 | 26.3 | **yes** (top-level in `core/utils.py:5`) |
| `bs4` | 35.5 | 26.1 | **yes** ← meta-warm (top-level in `communication.py`, `notifications.py`, `utils/pdf.py`) |
| `requests` | 34.6 | 25.2 | **yes** ← meta-warm |
| `openpyxl` | 34.6 | 25.2 | **no** (lazy) |
| `rq` | 33.9 | 24.5 | **yes** (eager; async-queue migration removes it) |
| `pyjwt` | 31.8 | 22.4 | **no** |
| `sentry_sdk` | 31.7 | 22.3 | **no** |
| `redis` | 31.0 | 21.6 | **yes** (eager) |
| `pyopenssl` | 30.4 | 21.0 | **no** |
| `pymysql` / `psycopg2` / `MySQLdb` | 29.6 / 21.1 / 17.2 | 20.2 / 11.7 / 7.8 | **no** (SQLite ⇒ unused — a real MariaDB→SQLite win) |
| `faker` | 28.3 | 18.9 | **no** |
| `cssutils` | 27.7 | 18.3 | **yes** ← meta-warm |
| `werkzeug` | 27.4 | 18.0 | **yes** (eager, core) |
| `html5lib` | 23.6 | 14.1 | **yes** ← meta-warm (transitive via bs4/cssutils — no direct Frappe import) |
| `charset_normalizer` | 19.2 | 9.6 | **yes** ← meta-warm |
| `cryptography` | 19.1 | 9.6 | **yes** (eager, core) |
| `pydantic` | 18.3 | 8.9 | **yes** (eager) |
| `lxml` | 18.1 | 8.7 | **yes** ← meta-warm |
| `pillow` (PIL) | 17.9 | 8.5 | **yes** ← meta-warm |
| `chardet` | 17.2 | 7.5 | **yes** ← meta-warm (direct top-level import at `email/receive.py:18`) |
| `pypika` | 14.8 | 5.4 | **yes** (eager, query builder) |
| `orjson` | 13.2 | 3.7 | **yes** (eager, first dep) |
| `hiredis` | 9.6 | 0.2 | **yes** (C-ext, ~free to import) |

(Full 63-module table in `measurements/isolation.csv`.) **Takeaways:** the heaviest *standalone* modules are
already **not resident** — good. **Two redundant charset libraries are both resident warm** — `chardet`
(17.2 MB, imported directly at `email/receive.py:18`) **and** `charset_normalizer` (19.2 MB, pulled via the
requests stack) — consolidating onto one is a free win. The actionable resident-and-deferrable cluster is the
**HTML/PDF/CSS group** (`pypdf`, `bs4`, `cssutils`, `markdownify`, `pdfkit`, + their transitive
`html5lib`/`soupsieve`/`lxml`/`PIL`), pulled in **only at first `get_meta`** by controller top-level imports.

---

## C. Per-module Python-heap — tracemalloc

Raw: `measurements/tracemalloc.txt` (and `measurements/README-tracemalloc-note.txt`). Python-object bytes
attributed to the allocating module, after `import frappe` and after warm-up. **Native/C-ext memory is
invisible here** (orjson, cryptography, lxml, PIL look tiny — their cost is native; see lens B).

| package | after import (MiB) | warm (MiB) | meta-phase Δ (MiB) |
|---|--:|--:|--:|
| `<frozen importlib._bootstrap_external>` | 19.00 | **34.09** | **+15.10** |
| `stdlib:enum` | 0.52 | 2.73 | +2.21 |
| `<frozen importlib._bootstrap>` | 1.11 | 2.69 | +1.58 |
| **`frappe.model`** | 0.14 | **1.95** | **+1.81** |
| `<frozen abc>` | 0.83 | 1.39 | +0.56 |
| `stdlib:encodings` | 0.00 | 1.37 | +1.37 |
| `site:redis` | 0.99 | 1.25 | +0.26 |
| `site:chardet` | 0.00 | 1.20 | +1.20 |
| `site:pypdf` | 0.00 | 1.17 | +1.17 |
| `site:pydantic` | 0.51 | 0.82 | +0.31 |
| **`frappe.database`** | 0.06 | 0.81 | +0.76 |
| `site:werkzeug` | 0.75 | 0.75 | +0.00 |
| `site:cssutils` | 0.00 | 0.69 | +0.69 |
| `site:bs4` | 0.00 | 0.60 | +0.60 |
| `frappe.utils` / `frappe.__root__` | 0.24 / 0.25 | 0.44 / 0.40 | +0.20 / +0.15 |

**Headline:** `<frozen importlib._bootstrap_external>` = **34 MiB** of Python heap warm. This is where
`marshal.loads` materialises each module's **top-level code object + the data constants baked into its
`co_consts`** (e.g. chardet's language-model tables) — i.e. roughly *the code of all 1,288 modules*, plus
importlib bookkeeping (337 k blocks). It grows **+15 MiB during meta-warm** precisely because +699 modules
load. Frappe's *own* Python objects are modest: `frappe.model` (meta/document machinery, +1.8 MiB on warm),
`frappe.database` (+0.76), `frappe.utils`, `frappe.__root__` — a few MiB total. **The meta jump is dominated
by loading more modules' code, plus a long tail of newly-imported packages** (chardet, pypdf, cssutils, bs4,
encodings…), *not* by metadata objects.

> The census `code_bytes` (lens D, ~21 MiB total) is **not** the same quantity as this 34 MiB and does not
> "redistribute" it — they differ by ~13 MiB. `bootstrap_external` is an **upper bound** that bundles every
> module's top-level code object **and** baked-in data constants; census `code_bytes` walks only
> functions/class-methods in each module's `__dict__` and never visits the `<module>` code object or its data
> constants. They are correlated ranking signals, not interchangeable totals.

---

## D. Module census — counts + code bytes per package

Raw: `measurements/census.txt`. Warm worker: **1,288 modules across 252 top-level packages.**

| top package | modules | code (MiB) |
|---|--:|--:|
| **frappe** (app) | 176 | **3.94** |
| redis | 57 | 1.64 |
| pydantic | 42 | 0.96 |
| asyncio | 30 | 0.75 |
| cssutils | 40 | 0.71 |
| werkzeug | 33 | 0.66 |
| pypdf | 48 | 0.65 |
| xml (stdlib) | 20 | 0.45 |
| PIL | 17 | 0.35 |
| pypika | 7 | 0.35 |
| html5lib | 13 | 0.30 |
| rq | 25 | 0.28 |
| requests | 92¹ | 0.27 |
| bs4 | 14 | 0.26 |
| chardet | 43 | 0.11 |

¹ `requests` shows 92 modules because it re-registers `chardet`/`urllib3`/`idna` under a second namespace
(`requests.packages.*`), so the *count* double-counts those aliases (real distinct ≈ 16). Their **code is
counted once** — by **code-object identity, first-package-touched wins** (a global `seen` set of `id(co)`);
this is dict-insertion-order-dependent but correct here because the real `urllib3`/`chardet` were inserted
before the requests aliases.

**Takeaway:** Frappe's own 176 modules are the biggest single code bucket (3.94 MiB), but the **other ~1,100
modules belong to dependencies** — redis (57), pypdf (48), chardet (43), pydantic (42), cssutils (40).
Because code objects track memory closely (lens C), trimming dependency imports is the lever.
`code_bytes` totals only ~21 MiB (~18 % of warm RSS) — use it to *rank* packages, not as a budget.

---

## E. The per-worker footprint — copy-on-write vs independent workers

Raw: `measurements/cow.jsonl`, `measurements/cow_summary.csv`; scripts `../scripts/fork_cow.py`,
`../scripts/independent.py`. Each worker warmed identically (init+connect+`get_meta` on 10 core doctypes).
PSS = proportional set size (shared pages divided among sharers) = the real footprint on the box; USS =
unique/private. **N = 4 workers** (+ parent in the fork model).

| Model | agg RSS (naive Σ) | **agg PSS (real box)** | per-worker private (USS) | parent `Shared_Dirty` |
|---|--:|--:|--:|--:|
| **Independent** (current: each worker imports frappe itself) | 452 MB | **351 MB** | **82 MB** | — |
| Pre-fork+COW, **idle** | 457 MB | **108 MB** | ~0.8 MB | 81 MB |
| Pre-fork+COW, **serving requests** (`do_work=1`) | 468 MB | **124 MB** | ~5 MB | 77 MB |
| Pre-fork+COW, serving + `gc.freeze()` | 467 MB | **123 MB** | ~5 MB | 77 MB |

**This is the decisive deployment result.** Four *working* workers cost **124 MB PSS** under pre-fork+COW vs
**351 MB** independent — a **~2.8× reduction (227 MB saved)** at light read load. **The savings erode with
write intensity** — a concurrent study using a *separate* harness (`../01-uss-pss-sharing.md`,
`../measurements/uss_pss_verified.md`) reproduced this matrix within noise and extended it:

| workload (4 forked children) | family PSS | per-child private | ratio vs 351 MB independent |
|---|--:|--:|--:|
| idle | 107–111 MB | 0.8–1.5 MB | **~3.2–3.3×** |
| + `gc.freeze()` | 107–111 MB | 0.8–1.5 MB | identical (freeze no help) |
| light single read/child *(this study's `do_work` cell ≈ 124 MB)* | 127–163 MB | 5–15 MB | ~2.2–2.8× |
| sustained realistic loop | 154–273 MB | 13–42 MB | ~1.3–2.2× |
| pathological `clear_cache` | 270–388 MB | 42–71 MB | ~0.9–1.3× (can **invert**) |

So **quote ~2× for capacity planning**, ~3× only for mostly-idle workers. The eroder is **CPython refcount
writes** into shared object headers (every `PyObject` touched), not GC traversal — which is why `gc.freeze()`
doesn't help and PEP 683 immortality (only `None`/`True`/`False`/small-ints −5..256) covers too little.
Mechanism confirmed by the single-worker smaps
breakdown (`../measurements/smaps_warm.txt`, analysed below): a warm worker's ~80 MB **anonymous heap**
(`[anon/heap]` 52.7 MB + `[heap]` 27.9 MB) is `Private_Dirty` when the worker is independent, but becomes
`Shared_Dirty` (COW-shared with the parent) when forked from a pre-warmed parent. Serving a request writes
CPython refcounts into a *few* of those shared object headers, dirtying only **~5 MB per child** — the other
~77 MB stays shared. `gc.freeze()` before fork added little here (the eroder is refcount churn, not GC
traversal).

Single-worker smaps (`smaps_warm.txt`, independent worker, KiB): `Rss 115820 · Pss 105075 ·
Shared_Clean 13228 · Private_Dirty 83904`. So for **independent** workers only the ~13 MB of file-backed
`.so`/libpython **code** is shared between siblings; the ~84 MB heap is fully private — which is exactly the
memory pre-fork COW reclaims. Largest private mappings: `[anon/heap]` 52.7 MB, `[heap]` 27.9 MB, then
C-extension `.so`s — `_rust.abi3.so` (cryptography) 5.9 MB, `etree…so` (lxml) 3.2 MB, `_pydantic_core` 3.0 MB,
`libcrypto` 4.2 MB, `_imaging` (PIL), `orjson`, `libsqlite3` — the native memory lens C can't see.

---

## The honest memory decomposition

The prior report's "interpreter ≈ 8 %, framework ≈ 92 %" holds. Refined three-way split using **clean
(non-tracemalloc) RSS** for totals and tracemalloc only for the Python-object share:

**Warm worker ≈ 114.5 MiB** (incremental 117,237 KiB / census 117,880 KiB — agree within 0.55 %):
- **interpreter baseline ≈ 9.4 MiB (8.2 %)** — isolation bare.
- **Python-heap objects ≈ 65.6 MiB (57.3 %)** — tracemalloc traced; per-package table corroborates within
  1.7 KB. Of this, **~34 MiB is module code objects** (the import machinery); framework's own objects are
  modest (`frappe.model` 1.95, `frappe.database` 0.81 MiB).
- **native/C-ext + fragmentation ≈ 39.5 MiB (34.5 %)** — **residual = UPPER BOUND**: it absorbs untraced
  interpreter-startup heap + glibc malloc arenas + true C-ext RSS (orjson-Rust, cryptography-OpenSSL,
  lxml-libxml2, PIL, sqlite3, hiredis). Do not over-interpret the absolute; the *ranking* (Python heap is the
  majority) is robust.

**After `import frappe` ≈ 59.5 MiB** (incremental cum 60,905 KiB / isolation 60,436 KiB — agree within 0.78 %):
interpreter ≈ 9.4 MiB (15.8 %) · Python heap ≈ 32.0 MiB (53.9 %, traced) · native/frag ≈ 18.0 MiB (30.3 %).

**First-`get_meta` jump:** +48.8 MiB real RSS as modules go 612→1288 (+676 from connect; +699 from
`import frappe`). ~61 % of the warm-band growth is Python heap, ~39 % native/frag from lazily-imported
C-extensions (lxml, PIL, cssutils, chardet, pypdf) dragged in by doctype controllers.

---

## Where the per-module levers are — ranked by measured payoff

1. **Pre-fork + COW (biggest, now measured: ~2× for planning, up to ~3.2× idle).** Warm `import frappe` +
   the common meta set in a parent and fork workers (gunicorn `--preload`). The ~80 MB framework heap is paid
   **once** and shared; light reads erode only ~5 MB/worker (124 MB vs 351 MB at N=4). This dominates every
   other lever and complements the async migration (fewer processes). **Caveat — erosion is real and
   load-dependent** (see the table in lens E and the concurrent `../01-uss-pss-sharing.md`): sustained
   meta-cache churn can erode the saving to ~1.3–2.2× and pathological `clear_cache` can invert it. `gc.freeze`
   does not help (the eroder is refcount writes, not GC). Plan for ~2×, re-measure under real traffic.
2. **Defer the HTML/PDF/CSS controller imports (biggest *import-trimming* lever).** `pypdf`, `bs4`, `cssutils`,
   `markdownify`, `pdfkit` are imported at **module top-level** in DocType controllers / `utils/pdf.py` /
   `print_format.py` / `communication.py` / `core/utils.py`, dragging in `html5lib`/`soupsieve`/`lxml`/`PIL`
   transitively. They arrive at the *first* `get_meta` and drive much of the +49 MB jump, yet a plain read or
   API request renders no PDF and scrubs no HTML. Move them to function-local imports.
   (Do **not** target `premailer` — already lazy — or `html5lib` directly — it has no Frappe import site;
   defer it by deferring `bs4`/`cssutils`.)
3. **Drop one of two redundant charset libraries.** `chardet` *and* `charset_normalizer` are both resident
   warm (17–19 MB standalone each, 43 + modules). Forcing the requests stack onto a single detector, and
   deferring the `email/receive.py:18` `chardet` import, removes a duplicate.
4. **Keep the heavyweights lazy — guard it in CI.** `IPython` (55 MB), `googleapiclient` (48), `posthog` (40),
   `sentry_sdk` (32), `faker` (28), `gitpython` (26) are declared deps **not** resident today. A test asserting
   they stay out of `sys.modules` after `import frappe`/first request prevents tens-of-MB regressions.
5. **Fewer modules ≈ less memory.** Code objects (~34 MiB) are the dominant Python-heap item and scale with the
   1,288-module count. Auditing the 589 eager imports and the 699 controller-triggered ones is the highest-
   leverage trimming work.
6. **SQLite already drops the DB drivers** (`pymysql`/`MySQLdb`/`psycopg2`, 17–30 MB standalone, not loaded) —
   a confirmed, environment-specific win of the MariaDB→SQLite workstream.

---

## Caveats & audit

Audited by 5 independent agents (methodology, arithmetic/consistency, attribution, completeness, synthesis).
Verdict: **methodology sound; headline numbers trustworthy** — incremental deltas reconcile exactly to
cumulative totals across all 3 runs; three independent methods agree on warm RSS within 0.55 % and
import-frappe within 0.78 %; the tracemalloc per-package table sums to its total within 1.7 KB. The
corrections it raised are all folded in above. Standing caveats:

- **tracemalloc RSS is contaminated — discarded.** Running under tracemalloc inflates RSS ~2.5× (warm RSS
  *under* tracemalloc = 294,600 KiB vs real ~117,000) because it stores a record per live allocation. The
  `native_gap_*` rows in `tracemalloc.txt` (104 MB / 232 MB) are **artifacts**, not native memory — flagged in
  `measurements/README-tracemalloc-note.txt` and relabelled `ARTIFACT_IGNORE_*` in the script. Only
  tracemalloc's *traced-object* totals are used.
- **The native/C-ext bucket (~40 MiB warm) is an UPPER BOUND**, not a measured native footprint (it absorbs
  untraced startup heap + malloc arenas + true C-ext RSS). The relative ranking is robust; the absolute isn't.
- **Incremental deltas are "retained-after-forced-GC"** (`gc.collect()` before each sample) — a lower bound on
  steady-state worker RSS. Practical gap is small because malloc arenas rarely return to the OS (warm 117,036
  matches census 117,880). init/connect deltas are post-GC `VmRSS`, ~0.4 MB below the `VmHWM` peak (captured).
- **Isolation re-counts shared transitive deps** (16× overcount if summed) and answers a hypothetical — its
  large rows (IPython, googleapiclient, premailer, posthog) are **not** resident. Always read it with the
  resident-Y/N flag.
- **census `code_bytes`** explains ~18 % of warm RSS, attributes shared code to the first package touched
  (order-dependent), and over-counts shared `co_consts`. A ranking metric, not a budget.
- **COW savings depend on traffic.** The ~65 % figure is for light reads (`do_work` = `get_all` + a few
  `get_meta`). Heavier per-request object mutation dirties more shared pages; re-measure under production load.
- **Single host, single warm path** (init+connect+`get_meta`×20 core doctypes + one `get_all`). A worker
  serving app-specific doctypes or actually rendering PDF/email/HTML loads **more** modules — so the
  lazy-import lever matters more, not less. SQLite site ⇒ the DB drivers are genuinely not resident
  (environment-specific).

---

## Raw data & scripts

- `scripts/measure_per_module.py` — `incremental` | `tracemalloc` | `census`
- `scripts/measure_isolation.sh` — fresh-process peak-RSS per module (min-of-3)
- `../scripts/fork_cow.py`, `../scripts/independent.py`, `../scripts/target.py`, `../scripts/analyze_smaps.py` — COW/PSS
- `measurements/incremental.csv` — 3 runs × 11 steps (cum RSS, Δ, `VmHWM`, module count)
- `measurements/isolation.csv` — 63 modules, standalone peak RSS (KiB), min-of-3
- `measurements/tracemalloc.txt` (+ `README-tracemalloc-note.txt`) — per-package Python-heap, totals, top 60 files
- `measurements/census.txt` — per-package module count + code bytes
- `measurements/cow.jsonl`, `measurements/cow_summary.csv` — COW matrix (4 cells) + independent baseline
- `../measurements/smaps_warm.txt`, `smaps_bare.txt` — single-worker smaps (mapping-level shared/private)
