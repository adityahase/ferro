# ferro-native — Frappe apps transpiled to Rust, one binary, no interpreter

> Can we **transpile Frappe app controllers to Rust and compile everything into a single binary** —
> no PyO3, no embedded CPython, no interpreter at all — run the 5 investigated apps on it, and stay
> under 64 MB? **Yes, demonstrated and measured.**

This is the aggressive successor to the embedded-CPython demo (`ferro-demo`/`ferrod`). Instead of
*running* the apps' Python on an in-process interpreter, we **compile their logic to native Rust**.

## What's here

| Path | What |
|---|---|
| `characterize.py` | AST survey of all 5 apps → how much is transpilable (the honest map) |
| `transpile.py` | the Python→Rust transpiler (`ast` → Rust source) |
| `../src/native/rt.rs` | the dynamic runtime the generated code calls |
| `../src/native/generated.rs` | **AUTO-GENERATED**: 1,274 transpiled methods + dispatch |
| `../src/native/main.rs` | the `ferro-native` binary (server + native write-driver) |
| `reports/01-transpilability.md` | corpus characterization & coverage |
| `reports/02-how-it-works.md` | the transpiler design, end-to-end |
| `measurements/results.md` | measured PSS/RSS/USS + the comparison table |
| `gen/selftest_cases.json` | the 7 cases proving transpiled logic runs |

## Headline results (measured on-host)

- **Single 2.09 MB binary, zero Python** — `ldd` shows only libc/libm/libgcc; no `libpython`, no
  Python symbols. Serves all 1,077 DocTypes' CRUD.
- **1,274 controller methods transpiled to Rust** and compiled in; **162 lifecycle handlers
  (validate/on_*/…) across 133 DocTypes** are wired and runnable (**311 methods reachable** = handlers
  + helpers; the other 963 are transpiled `@whitelist` RPCs not yet routed — see the audit).
- **Transpiled logic provably executes** — 7/7 selftests + 5/5 runtime unit tests: validation throws
  fire on field values, `{0}`-formatting works, `maximum_use=1` is *computed and persisted*,
  conditionals discriminate, pure-CRUD still serves non-native DocTypes.
- **Memory:** peak **18.8 MB RSS @4 threads (tuned)** / 23.4 MB (default) under mixed read +
  transpiled-write load — **~3× under 64 MB**, **~2.7–3.3× lighter than the PyO3 path** (ferrod,
  63 MB), **~5–6× lighter than stock Frappe** (~115 MB).
- **Adversarially audited** (8-agent workflow): memory & no-Python claims independently confirmed;
  7 correctness bugs found and fixed + regression-tested. See `reports/03-audit.md`.

## Reproduce

All commands below are run from the repo root. The Frappe app clones are a bring-your-own
input: clone the 5 apps into `docs/investigations/apps-64mb/repos/` (gitignored) or point
`$FERRO_REPOS` at them. The demo site is built by `demos/pyo3-apps/build_db.py`.

```bash
# 1. transpile the apps -> src/native/generated.rs
python3 transpile/transpile.py

# 2. compile the single binary (no python feature, links only libc)
cargo build --release --features native --bin ferro-native
ldd target/release/ferro-native | grep -i python   # -> nothing

# 3. prove the transpiled logic runs
target/release/ferro-native selftest demos/pyo3-apps/site

# 4. measure memory under load
target/release/ferro-native loadtest demos/pyo3-apps/site --threads 4 --rounds 8

# 5. serve the real REST API (curl it like a Frappe worker)
target/release/ferro-native serve demos/pyo3-apps/site --port 8080
```

## The key insight

Compiled code costs ~0 resident memory (it lives in the shared, read-only text segment); interpreted
code costs per-process heap. So the more app logic we move from Python to Rust, the closer the whole
system sits to ferro's ~5 MB floor — and **memory is decoupled from coverage**: even partial
transpilation delivers the full memory win for the covered set, with the uncovered tail falling back
to the PyO3 path or staying deferred-and-documented.

See `reports/02-how-it-works.md` for the design (uniform `serde_json::Value` model, the scope-hoisting
and borrow-temp fixes, the selective write-driver) and `reports/03-audit.md` for the adversarial audit.
