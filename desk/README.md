# desk/ — Frappe Desk compatibility oracle

Evidence that the pure-Rust `ferro --desk` runtime serves the real Frappe Desk admin SPA
(workspaces / lists / forms / writes) against a SQLite site, with no Python — validated
against a captured "oracle" of the genuine Frappe v17 Desk.

| File | What |
|---|---|
| [`REPORT.md`](REPORT.md) | the compatibility report: 18-route sweep, zero console errors, every bug fixed, honest limitations |
| [`SPEC-synthesis.md`](SPEC-synthesis.md) | the deep spec — the 16 Desk methods mapped onto the ORM, with implementation status |
| [`SPEC-subsystems.json`](SPEC-subsystems.json) | structured boot-subsystem spec (status + notes per key) |
| `run-desk.sh` | build `ferro` (release) and serve Desk on `:8001` against a bench's SQLite site |

```bash
./desk/run-desk.sh            # then open http://localhost:8001/app
# override: SITE=/path/to/site.sqlite PORT=8080 ./desk/run-desk.sh
```

**The captured oracle is intentionally not committed.** The `oracle/` (≈2.5 MB of real-Desk
API responses, request logs, and ferro-vs-real screenshots) and `baseline/` (boot + rendered
HTML snapshots) directories are bulky, regenerable test fixtures — they are gitignored. To
regenerate them, serve both the real bench and `ferro --desk`, then drive each with a headless
browser and capture the API traffic. The report quotes the figures that matter.

The Desk serving itself lives in the runtime at [`../src/desk.rs`](../src/desk.rs).
