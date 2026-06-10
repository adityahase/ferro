# ferro

A drop-in Rust replacement for the CPython + Frappe web worker — same REST API, same SQLite site, no
Python, under 64 MB (vs ~115 MB).

## Why

CPython is only ~3.4 MB of a worker; the rest is Frappe's import + object graph. ferro reclaims it by
not having it. → [docs/memory.md](docs/memory.md)

## Run

```bash
cargo build --release --bin ferro
target/release/ferro serve /path/to/site --port 8000 [--desk]   # REST (+ Desk SPA), no Python
contrib/bench-ferro-switch.sh on                                 # swap into a bench; `off` reverts
```

The swap flips one reversible flag — `web_runtime: gunicorn → ferro` in `common_site_config.json`. →
[docs/comparison-with-bench.md](docs/comparison-with-bench.md)

## Scope

- **Works, pure Rust:** v1 + v2 REST (incl. bulk / copy / expand / doctype meta+count), password login
  & real `tabSessions`, row + field-level permissions (permlevel masking + DocShare), naming, Desk,
  app SPAs (crm/helpdesk/gameplan).
- **Needs `ferrod`** (`--features python`): controller logic — `validate`/`on_submit`, submit / cancel /
  workflow, app methods.
- **Not yet:** production one-flag swap, doc-level User Permissions. → [docs/LIMITATIONS.md](docs/LIMITATIONS.md)

## More

[architecture](docs/architecture.md) · [memory](docs/memory.md) · [cli](docs/cli.md) ·
[all-in-one backend](docs/all-in-one-backend.md) · [report](REPORT.md) ·
[investigations](docs/investigations/README.md)

MIT — see [LICENSE](LICENSE).
