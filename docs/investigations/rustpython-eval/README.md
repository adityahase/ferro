# rustpython-eval — evaluated and rejected

Before committing to a from-scratch Rust runtime, we evaluated whether an existing
alternative Python implementation could simply run Frappe with a smaller footprint.
**RustPython 0.5.0** (a Python interpreter written in Rust) was built and tested.

**Verdict: rejected.** It cannot run Frappe:

- **No `sqlite3`** — RustPython ships no `sqlite3` module, and Frappe's data layer needs it.
- **No CPython C-API** — so C-extension dependencies Frappe relies on (e.g. `orjson`) cannot load.
- **No `pip`** — the dependency tree can't be installed.

The same dead-ends apply to PyPy (C-API/packaging friction for Frappe's stack) and MicroPython
(missing stdlib). The conclusion — *CPython is the only viable interpreter; the win has to come
from a different runtime, not a different Python* — is what motivated `ferro`. See the
[runtime-memory findings](../runtime-memory/00-FINDINGS.md) for the full reasoning.

The 34 MB RustPython binary itself was a throwaway build artifact and is **not** vendored here.
