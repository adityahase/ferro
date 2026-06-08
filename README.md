# ferro

A Rust runtime that replaces the ~115 MB CPython + Frappe REST worker and serves the **same
Frappe v1 API**, against the **same** SQLite site, **under 64 MB** — idle ~4.6 MB, ~17.7 MB peak
under heavy concurrent load at the default thread count (≈85% less than a warm Frappe worker).
The release binary is 1.7 MB with three dependencies (`rusqlite`, `tiny_http`, `serde_json`).

```bash
cargo build --release --bin ferro
target/release/ferro serve /path/to/site --port 8000
# serve the Frappe Desk admin SPA too (pure Rust, no Python):
target/release/ferro serve /path/to/site --port 8000 --desk
```

See **[REPORT.md](REPORT.md)** for the full measurement + fidelity audit, and
**[docs/LIMITATIONS.md](docs/LIMITATIONS.md)** for the intentional gaps.

## The runtime — one source tree, three binaries

| Binary | Build | What it adds |
|---|---|---|
| **`ferro`** | `--bin ferro` | pure Rust: REST + Desk + auth/permissions/naming against SQLite. No interpreter. |
| **`ferrod`** | `--features python --bin ferrod` | embeds CPython (PyO3) so writes drive the **real** Frappe app controllers. |
| **`ferro-native`** | `--features native --bin ferro-native` | controllers **transpiled** Python→Rust and compiled in — controller logic with **zero** libpython. |

The shared data plane lives in [`src/`](src/) (`main.rs` routing · `orm.rs` · `meta.rs` ·
`auth.rs` · `crypto.rs` dependency-free Fernet · `naming.rs` · `desk.rs` · in-process
`cache.rs`/`jobs.rs`/`realtime.rs` — see [docs/all-in-one-backend.md](docs/all-in-one-backend.md)).

## Repository map

Most important first — the runtime, then how to run it, then the apps tracks, then deployment,
then the research it all rests on.

| Path | What |
|---|---|
| [`src/`](src/) · `Cargo.toml` | **the Rust runtime** — the three binaries above |
| [`REPORT.md`](REPORT.md) · [`measurements/`](measurements/) | the headline report + the fidelity (41/41) & memory harnesses |
| [`cli/`](cli/) | the bench-style `ferro` CLI (a `bench` analog, python3 stdlib) + setup/bootstrap scripts |
| [`framework/`](framework/) | the native-backed Python `frappe` shim, schema tooling, and the 591 KB frappe-core SQLite seed |
| [`transpile/`](transpile/) | the Python→Rust controller transpiler that generates `src/native/generated.rs` |
| [`demos/pyo3-apps/`](demos/pyo3-apps/) | the working demo: 5 Frappe apps (crm/helpdesk/gameplan/hrms/erpnext) on `ferrod`, under 64 MB |
| [`desk/`](desk/) | the Frappe Desk compatibility oracle + report (`ferro --desk` serves the real admin SPA) |
| [`contrib/`](contrib/) | `bench-ferro-switch.sh` — reversibly swap ferro into a real Frappe bench's Procfile |
| [`deploy/signup/`](deploy/signup/) | the self-serve signup control plane (provisions Desk tenants behind Caddy) |
| [`docs/`](docs/) | [architecture](docs/architecture.md) · [memory](docs/memory.md) · [cli](docs/cli.md) · [comparison-with-bench](docs/comparison-with-bench.md) · [investigations/](docs/investigations/) |

## How it came together

A measure → build → run-apps → deploy arc; each step's evidence is in
[`docs/investigations/`](docs/investigations/README.md).

1. **Measure (why a rewrite at all).** The ~115 MB warm worker is ~92% Frappe object graph, not
   interpreter, and no alternative Python (RustPython/PyPy/MicroPython) can run Frappe — so the win
   had to come from a different *runtime*, not a different Python.
   → [investigations/runtime-memory](docs/investigations/runtime-memory/00-FINDINGS.md),
   [rustpython-eval](docs/investigations/rustpython-eval/README.md)
2. **Build the runtime.** `ferro` serves the v1 REST API directly off SQLite at ~18 MB peak;
   fidelity-audited (57 divergences fixed), dependency-free Fernet auth, `naming_series`,
   `if_owner`/permlevel permissions. → [REPORT.md](REPORT.md)
3. **Run real apps.** The +106 MB framework import cliff is avoided when ferro *is* the framework
   and apps use a thin shim — proven two ways: `ferrod` (embedded CPython, real controllers, ~46 MB
   peak) and `ferro-native` (controllers transpiled to Rust, ~18.8 MB, no libpython).
   → [investigations/apps-64mb](docs/investigations/apps-64mb/00-ARCHITECTURE.md),
   [demos/pyo3-apps](demos/pyo3-apps/00-DEMO.md), [transpile](transpile/README.md)
4. **Deploy.** Drop ferro into a real bench ([contrib/](contrib/), the
   [drop-in study](docs/investigations/drop-in/00-REPORT.md)), or run the self-serve signup that
   provisions ~15–25 MB Desk tenants behind Caddy ([deploy/signup](deploy/signup/README.md)).

## License

MIT — see [LICENSE](LICENSE).
