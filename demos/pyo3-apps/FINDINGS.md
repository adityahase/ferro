# Findings — running the Frappe app ecosystem on ferro under 64 MB

**Date:** 2026-06-07/08 · **Author:** autonomous build session
**Scope:** Turn the `docs/investigations/apps-64mb` *research* into a *working, measured* demo of the
five apps (crm, helpdesk, gameplan, hrms, erpnext) running on ferro, and confirm (or refute) the
64 MB budget with real PSS/RSS/USS. Then compress where it didn't fit.
**Companion docs:** `00-DEMO.md` (full report), `README.md`, `RESULTS.txt` (raw matrix),
`VERIFY.txt` (functional proof).

---

## 1. Headline findings

1. **The integration was unbuilt; it is now built and runs.** Before this session, ferro was a
   pure-Rust REST server with *no* Python. I added `ferrod` = ferro's Rust data plane **+ embedded
   CPython 3.13 (PyO3)** + a native `frappe` shim + a controller loader/dispatcher. All five apps'
   ~800 doctypes are served; **798/799 controllers register and run their own Python**.

2. **The apps genuinely run** — verified 8/8 in-process *and* over HTTP. Reads are served 100% in
   Rust (no GIL); writes drive the **real** app controllers in embedded CPython. Clean proof: hrms
   `RetentionBonus.validate` rejects a past `bonus_payment_date` with its **exact source message**
   (`HTTP 417 "Bonus Payment Date cannot be a past date"`), child tables persist, then ferro's
   Rust mandatory-field check still fires when the controller passes.

3. **64 MB holds for the realistic deployment, not for the pathological one.** With a *realistic*
   load (reads of 20 rows × all columns from populated tables; writes incl. child rows):

   | mode | idle RSS | peak RSS @ 1 / 2 / 4 / 8 threads |
   |---|--:|--:|
   | **lazy (recommended)** | 26 MB | 30 / 36 / **46** / 65 MB |
   | eager (all 779 resident) | 50 MB | 55 / 60 / 70 / 91 MB |

   → **Lazy loading + ≤4 worker threads runs all five apps under 64 MB (≤46 MB peak).** Eagerly
   resident-loading *every* controller, or running ≥8 threads, exceeds 64 MB. Memory scales
   ~+5 MB/thread and ~linearly with the number of resident controllers.

4. **The win is structural, not a tuning trick.** ferro *being* the framework removes the
   measured **+106 MB framework import cliff** entirely. The Python side carries only the
   interpreter (~12 MB) + a thin shim + the app controllers actually touched. For reference: a real
   CPython+Frappe worker is ~115–155 MB; pure-Rust ferro is ~8 MB; the parallel transpiled
   `ferro-native` is ~18 MB.

---

## 2. What it took (technical findings)

- **PyO3 embedding is viable here.** Python 3.13.13 has a shared `libpython`; pyo3 0.23 links and
  initialises it. Bare embedded interpreter = **12 MB RSS** (4 MB pre-init). Toolchain note: the
  env's `python3` is 3.14 but only 3.13/3.12 have an embeddable shared lib — used 3.13.13.
- **Connection sharing across Rust→Python→Rust is sound** with a thread-local `*const Connection`
  set around each GIL-held dispatch: the GIL serialises Python, and each pure-CRUD Rust thread uses
  its own connection, so no aliasing. (Audited — confirmed safe.)
- **Import-completeness needed a permissive lazy shim.** Real controllers import deep `frappe.*`
  internals and heavy optional deps that ferro doesn't provide. A subclassable/operator-absorbing
  `Stub` + a meta-path finder (appended last, so real modules win) got **0 import errors across all
  779 controllers**. The honest cost: where a controller's *runtime* logic reaches such a stub, it
  aborts that branch (now logged).
- **~53% of doctypes are pure-CRUD** (zero controller logic) → served entirely in Rust. The
  needs-Python tail is concentrated in transactional doctypes; `'*'` wildcard hooks (erpnext
  `validate`, gameplan `on_trash`) mean every *write* consults Python when those apps are loaded —
  reads stay in Rust regardless.
- **Lazy loading is the operative memory lever** (as the architecture predicted): build the
  needs-Python registry by AST-scanning controllers (no import), import a controller only on first
  hooked write. Idle 50→26 MB, 4-thread peak 70→46 MB.
- **Allocator tuning matters at the margin:** jemalloc `dirty_decay_ms:0,narenas:1,tcache:false`
  returns freed per-request pages and removes per-thread arena fragmentation; 256 KB SQLite page
  cache (from 2 MB) is the single biggest per-thread saving; `PYTHONNODEBUGRANGES=1` trims code
  objects.

---

## 3. Adversarial audit — 14 confirmed findings and their disposition

A multi-agent audit (each finding independently verified by trying to refute it) raised 18
findings, 14 confirmed. Disposition:

| # | Sev | Finding | Disposition |
|---|---|---|---|
| 1 | high | "64 MB ceiling" was 4-thread/empty-table specific; breaks at 8T (78 MB) | **FIXED (docs)** — re-measured to 8T, stated the real envelope (lazy ≤4T) |
| 2 | med | Load-test read 10/12 *empty* tables, name-only — understated peak | **FIXED** — `populate_demo_data.py` (3000 rows × all cols), reads now 20 rows × `*`; re-measured |
| 3 | low | Idle PSS/USS ~1 MB optimistic vs current binary | **FIXED (docs)** — noted ±1 MB run-to-run variance |
| 4 | crit | `CRMSalesHierarchy.validate` cited as proven but never ran (NestedSet base stubbed) | **FIXED** — added NestedSet base; removed the false citation; `RetentionBonus` is the clean proof |
| 5 | crit | 20 master doctypes silently lost all controller logic; mislabeled "data tables" | **FIXED** — NestedSet/WebsiteGenerator bases → 798/799 register; corrected the claim |
| 6 | high | `_run_event` swallowed non-Validation exceptions → validate could silently no-op | **FIXED** — now records + logs (`FERRO_LOG_SWALLOWED=1`); revealed controllers partially abort on stubs |
| 7 | high | Honesty section disclosed wrong boundary, omitted class-level dropping | **FIXED (docs)** — rewritten to disclose base-class drops + partial aborts |
| 8 | crit | Python dispatch dropped **all child-table rows** on insert/update (data loss) | **FIXED** — `get_valid_dict()`→`as_dict()`; verified (2 contacts persist) |
| 9 | high | Generated non-child tables carry bogus `parent`/`parentfield`/`parenttype` | **DISCLOSED** — kept (ferro `STANDARD_COLUMNS` expects them); noted cosmetic divergence |
| 10 | med | `override_doctype_class` parsed but never instantiated/counted | **DISCLOSED** — overridden doctypes use the original controller |
| 11 | med | Lazy AST registry can miss *inherited* lifecycle methods | **DISCLOSED** — lazy is conservative; eager (MRO) is exact |
| 12 | low | submit/cancel/rename hooks can never fire (no route) | **DISCLOSED** — out of v1 `/api/resource` scope |
| 13 | med | DEMO mis-attributed `'*'.on_trash` to erpnext (it's gameplan) | **FIXED (docs)** |
| 14 | low | Lazy registry called "exact" but over-counts (346 vs 329) | **FIXED (docs)** — "conservative over-approximation" |

**Net:** 3 critical/high *code* bugs fixed (child-table loss, base-class drops, silent swallow);
the rest were doc overstatements, now corrected or disclosed.

---

## 4. Honest limitations (what "the apps run" does and doesn't mean)

- Reads and pure-CRUD writes are **fully real** (Rust).
- For needs-Python doctypes, the controller's **own** business logic runs **to the extent it
  doesn't reach an un-ported framework internal**. Where it does — e.g. `CRMDeal.before_validate`
  builds an SLA query through the *partial* `frappe.qb` and raises — that branch aborts (now
  logged) and is treated as a no-op. So app **business rules** run; framework **side-effects**
  (email/PDF/search/realtime, the full query DSL) belong to an out-of-budget worker, by design.
- `RetentionBonus.validate` is the clean end-to-end proof because it is self-contained
  (`getdate` + own fields). Controllers that lean on deep `frappe.*` are partially exercised.
- Dispatch runs `validate` before naming resolves `name` (Frappe names first); `name`-based
  self-checks in `validate` see a blank name.
- Other write semantics follow `ferro/LIMITATIONS.md` (child-row name preservation, optimistic
  concurrency, link-integrity on delete).

---

## 5. Reproduce

```bash
python3 build_db.py && python3 populate_demo_data.py 3000
cd /home/frappe/ferro && PYO3_PYTHON=/home/frappe/.pyenv/versions/3.13.13/bin/python3 \
  cargo build --release --features python --bin ferrod
bash ./bench.sh    # -> RESULTS.txt (memory matrix)
bash ./verify.sh   # -> VERIFY.txt  (8/8 functional)
```

---

## 6. Related tracks (not this build)

- `/home/frappe/ferro/` — the underlying pure-Rust ferro runtime (data plane).
- `docs/investigations/apps-64mb/` — the prior feasibility research this demo realises.
- `transpile/` + `src/native/` — a **separate, parallel** effort
  transpiling controllers directly to Rust (no interpreter; 2.09 MB binary, ~18 MB peak, 64% of
  doctypes fully native-able). Referenced, not modified by this build.
