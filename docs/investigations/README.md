# Investigations — the research behind ferro

The measurement and feasibility work that motivated and shaped ferro, in the order the
questions were asked. The findings docs (`00-*.md`) are the output; the `scripts/` are the
reusable harnesses; the bulky raw dumps (heap captures, full smaps, the 353 MB app clones,
the 34 MB RustPython binary) are **not** vendored — the conclusions live in the prose.

| Investigation | Question | Verdict |
|---|---|---|
| [**runtime-memory/**](runtime-memory/00-FINDINGS.md) | Can a different runtime shrink the ~115 MB CPython+Frappe worker? | **No** — CPython is the only viable interpreter; ~92% of the worker is framework object graph, not interpreter. Levers measured (pre-fork+COW, lazy imports, `malloc_trim`). |
| ↳ [per-module/](runtime-memory/per-module/00-PER-MODULE-FINDINGS.md) | Where does the warm worker's memory go, per module? | Code objects ~34 MB; a +49 MB "meta jump" is a lazy-import avalanche; pre-fork+COW ~65% saving at N=4. |
| ↳ [granular/](runtime-memory/granular/00-GRANULAR-FINDINGS.md) | Account for every MB of the 155 MB warm worker. | obmalloc 65 / glibc-heap 53 / `.so` 35; import graph 43. Ranked levers; `gunicorn --preload` is the biggest. |
| [**rustpython-eval/**](rustpython-eval/README.md) | Could RustPython/PyPy/MicroPython run Frappe instead? | **No** — no `sqlite3`, no C-API (`orjson`), no `pip`. This is what pushed us to a Rust runtime. |
| [**apps-64mb/**](apps-64mb/00-ARCHITECTURE.md) | Can the Frappe *app* ecosystem run on ferro in 64 MB? | **Yes, measured** — the +106 MB framework import cliff is avoided when ferro *is* the framework and apps import a thin shim. |
| [**drop-in/**](drop-in/00-REPORT.md) | Can ferro be a faithful drop-in inside a real bench? | Distance MODERATE; a faithful drop-in needs **ferrod** (Rust fast-path + PyO3 fallthrough). Phase 0 landed (`--bench-mode` + reversible Procfile switch). |

These directly produced the runtime (`../../src/`), the apps tracks (`../../demos/`,
`../../transpile/`), and the bench-switch helper (`../../contrib/bench-ferro-switch.sh`).

> Note on app clones: `apps-64mb/floor_measure.py` (and the transpiler) read the 5 Frappe apps'
> source from `$FERRO_REPOS`, defaulting to `apps-64mb/repos/` — a bring-your-own, gitignored
> location. Clone crm/helpdesk/gameplan/hrms/erpnext there to re-run the measurements.
