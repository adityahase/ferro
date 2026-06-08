# Sources & evidence

## Primary (verified on this host, 2026-06-06)

All numbers in `00-FINDINGS.md` come from measurements taken on this machine, not from the web:

- **Interpreter baselines:** `scripts/baseline_final.sh` → `measurements/baseline_final.csv`
  (peak RSS via `/usr/bin/time -f '%M'`, min of 3 runs, identical harness per interpreter).
- **Frappe footprint:** `scripts/measure_frappe.sh` → `measurements/frappe_cpython314.csv`,
  plus the extended min-of-3 scenarios in the session log.
- **Compatibility proofs (empirical):**
  - PyPy can't parse Frappe source: `uv pip install -e apps/frappe` under PyPy 3.11 →
    `SyntaxError: invalid syntax` on `type ConfType = _dict[str, Any]` (PEP 695, 3.12+).
  - PyPy `requires-python` gate (when testing v17 metadata): `Python>=3.14,<3.15` vs PyPy 3.11.
  - `orjson` won't build on PyPy: `uv pip install orjson` → maturin/PyO3 build fails.
  - RustPython: `rustpython -m pip` → `No module named pip`; `import frappe` →
    `ModuleNotFoundError: No module named 'orjson'` at `frappe/__init__.py:27`;
    `import sqlite3` → `No module named '_sqlite3'`.
  - MicroPython: `import datetime|logging|sqlite3|functools|itertools|importlib|base64` all
    `ImportError`; `help("modules")` shows an embedded module set only.
  - Frappe's first third-party import is `orjson` at `frappe/__init__.py:27`; Frappe declares
    **73 direct dependencies** in `apps/frappe/pyproject.toml`.

## Secondary (web, for context only — used to plan the bench, not for the conclusions)

- Frappe SQLite support is on the **develop** branch via `bench new-site --db-type sqlite`:
  - https://discuss.frappe.io/t/initial-sqlite-support/145830
  - https://docs.frappe.io/framework/v15/user/en/bench/reference/new-site
  - https://github.com/frappe/frappe/issues/1333 (Feature: SQLite Integration)
- Frappe Python version support:
  - v15: `requires-python = ">=3.10,<3.14"` —
    https://docs.frappe.io/framework/user/en/installation
  - **develop (17.0.0-dev): `requires-python = ">=3.14,<3.15"`** — observed directly from the
    cloned `apps/frappe/pyproject.toml` during `bench init` (uv resolution error).
- Runtime projects:
  - CPython — https://www.python.org/
  - PyPy — https://www.pypy.org/  (compatibility: https://doc.pypy.org/en/latest/cpython_differences.html)
  - MicroPython — https://micropython.org/  (docs: https://docs.micropython.org/)
  - RustPython — https://github.com/RustPython/RustPython
  - orjson (CPython-only, PyO3) — https://github.com/ijl/orjson

> Note: a background research workflow was started to cross-check these but was interrupted by a
> session pause; the conclusions here rest on the on-host empirical evidence above, which is
> stronger than any web claim for this specific question.
