# Frappe Memory-Reduction: Verification + Feasibility Report

## 1. Claim Verification

| # | Claim (abridged) | Verdict | Recomputed |
|---|------------------|---------|------------|
| 1 | Warm worker RSS 154.4 MB splits: obmalloc anon ~65.4 / glibc [heap] ~53.1 / libs+binary ~35.7 / misc ~0.3 MB | **CONFIRMED** | anon 65.45, heap_brk 53.08, lib+file 35.73, stack+special 0.10; total 154.4 MB (all within 0.21 MB) |
| 2 | obmalloc = 64 arenas × 1 MiB = 64 MiB reserved, 97% full (65.2 MB live); arena = 1 MiB NOT 256 KiB | **CONFIRMED** | 64 × 1,048,576 = 67,108,864 B (64.0 MiB); blocks 65,222,464 B; fill 97.20%; arena size = 1,048,576 B exactly |
| 3 | tracemalloc top site 43.4 MB at `<frozen importlib._bootstrap_external>:511` (.pyc unmarshal); traced ~83.7 MB | **CONFIRMED** | 45,533,294 B = 43.4 MB at line 511; traced_current 83.74 MB |
| 4 | Live-object census ~68.0 MB; code 15.0 (39,657), str 13.7, dict 13.1, function 6.7, type 6.3 MB | **CONFIRMED** | total 68.0 MB; code 15.0 / str 13.7 / dict 13.1 / function 6.7 / type 6.3 MB — exact |
| 5 | glibc mallinfo2: arena 54, in-use 30.2, fordblks 23.8 MB; malloc_trim ~22 MB; obmalloc separate | **CONFIRMED** | [heap] Rss 53.1 MB ≈ arena 54.0 (0.9 MB unfaulted brk); trim 154.4→132.5 = 21.9 MB; obmalloc 65.4 MB separate |
| 6 | Workload loads 1730 modules vs **1288** idle (real loads MORE); lazy-stub path drops to 1320 | **PARTIAL** | Workload 1730 > import baseline 1620 (+110) confirms "MORE". But **1288** not in provided files; **1320** lazy-stub figure unverifiable (needs live run) |

### Flag — what to fix on the PARTIAL (Claim 6)
- The "MORE usage loads more" direction holds (1730 vs 1620 baseline).
- **Fix the baseline number**: the claim cites `1288`, but the available `import.summary.json` reports `1620`. Either reconcile `1288` to the historical prior study it came from (cite that artifact explicitly) or restate the baseline as 1620.
- **Substantiate or drop the `1320` lazy-stub figure** — it is not reproducible from the read-only artifacts; rerun the stubbed serve path and capture `gc.n_modules` before publishing.

## 2. Lazy-Import Feasibility (ranked easiest → hardest)

| Rank | File / deps | Deferrable | Proposed change | Risk |
|------|-------------|-----------|-----------------|------|
| 1 | `frappe/utils/scheduler.py` — croniter | **YES** | Move `from croniter import CroniterBadCronError` into `enqueue_events()` body | Negligible — leaf dep, scheduler-only, off request path |
| 2 | `frappe/search/website_search.py` — whoosh | **YES** | Move `from whoosh.fields import ID, TEXT, Schema` into `get_schema()` | Very low — parent already loads whoosh; no class-body use |
| 3 | `frappe/monitor.py` — rq (→redis, cryptography) | **YES** | Move `import rq` into `collect_job_meta()`; pattern already used in `utils/error.py:87` | Low — job-only path, no circular import |
| 4 | `frappe/integrations/oauth2.py` — oauthlib | **YES** | Move imports into `get_oauth_server()` and the 5 whitelisted handlers (or shared helper) | Minimal — request-time only, no circular import |
| 5 | `frappe/app.py` — babel, num2words | **YES** | Delete redundant module-level imports; `utils/data.py` already lazy-imports them per-function | Minimal — pure COW/`gc.freeze` pre-load; remove only affects post-fork sharing, not correctness |
| 6 | `frappe/database/mariadb/schema.py` — pymysql | **YES** *(RISKY)* | Move `from pymysql.constants.ER import DUP_ENTRY` into `alter()` before line 169 | Risky-labeled but low in practice — only loaded via `get_db()` for MariaDB; safe if no other import path |
| 7 | `frappe/utils/pdf.py` — cssutils, pdfkit, bs4, pypdf | **RISKY** | Wrap all imports + side effects (`pdfkit.source.unicode`, `cssutils.log.setLog`, `FrappePDFKit` monkeypatch) in a cached `_init_pdf_utils()`; rewrite type hints | **HIGH** — module-level monkeypatching (lines 51-53), `cssutils` log setup, and type hints (`PdfWriter`, `BeautifulSoup`) evaluated at def time; needs `from __future__ import annotations` |
| 8 | `frappe/core/doctype/file/file.py` — PIL | **NO** | Cannot defer | **BLOCKING** — `ImageFile.LOAD_TRUNCATED_IMAGES = True` is required at import; deferring breaks image handling for any earlier PIL user |

### Net feasibility
- **6 of 8 deferrable** (5 clean + 1 conditionally-safe MariaDB). Ranks 1-5 are low-effort, low-risk wins.
- **pdf.py** is deferrable only with real refactoring (move monkeypatch into a lazy initializer + `__future__` annotations); treat as a separate, tested change.
- **file.py** is a hard NO — keep eager.

## 3. Bottom Line

The headline findings **survive scrutiny**:

- **Worker ~155 MB** — CONFIRMED at 154.4 MB, and the four-way split (obmalloc 65.4 / glibc heap 53.1 / libs 35.7 / misc 0.3) reconciles to the total within 0.21 MB.
- **Import graph ~43 MB** — CONFIRMED: the single largest tracemalloc site is exactly 43.4 MB of `.pyc` unmarshal (code objects + constants), corroborated by the 68 MB live census where `code` is the top type (15 MB / 39,657 objects).
- **Allocator headroom -4..22 MB** — CONFIRMED on the upper bound: `malloc_trim` reclaims 21.9 MB of glibc `fordblks` (23.8 MB free-retained); obmalloc is independently 97% full, so the slack is genuinely in the glibc arena, not obmalloc.
- **Lazy-import -28 MB** — *directionally supported but not independently re-derived here.* The feasibility audit confirms enough safe deferrals exist to move real module weight off the import path (6/8 candidates), but the specific 28 MB delta rests on the `1730 → 1320` module-count drop, whose **1320 endpoint is unverified** and whose baseline citation (`1288` vs measured `1620`) is inconsistent.

**One caveat to close before publishing:** reconcile the module-count baseline in Claim 6 and reproduce the 1320 lazy-stub figure with a live run. Every allocator/import/RSS number checked out exactly; only the lazy-import *magnitude* still leans on an unverified count.