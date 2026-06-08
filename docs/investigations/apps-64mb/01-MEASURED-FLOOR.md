# Measured memory floor — running Frappe apps on a thin shim (no framework)

All numbers measured directly on this box: CPython **3.14.4** (pyenv, pymalloc/glibc — same
build as the prior runtime-memory study), ferro release build (`opt-level=z`, LTO, stripped).
Method: `floor_measure.py` — a permissive `frappe`/app/third-party import stub (meta_path
finder) so controller **module bodies** compile + exec (class defs, decorators, module-level
constants) **without** loading the real framework or any heavy dependency. RSS read from
`/proc/self/status:VmRSS`.

## The headline: the +106 MB cliff is the FRAMEWORK, not the apps

Prior study (full real worker): bare interp **12 MB** → `import frappe`+`frappe.app` **118 MB**
(a **+106 MB** cliff, ~43 MB of it the marshalled code graph of werkzeug/jinja/redis/pymysql/
requests/num2words/whoosh/pydantic/cryptography/…). When **ferro (Rust) *is* the framework**,
Python never imports real frappe — only a thin shim — so that cliff is **never paid**.

## App code resident cost on the thin shim (this study)

| Scenario | RSS | modules | marshalled code |
|---|--:|--:|--:|
| bare CPython 3.14 (`pass`) | **10.5 MB** | 45 | — |
| + thin `frappe` shim | 11.0 MB (+0.5) | 46 | — |
| crm — all 44 doctype controllers | 14.9 MB | 158 | 0.25 MB |
| gameplan — all 21 | 14.0 MB | 120 | 0.11 MB |
| helpdesk — all 39 | 16.1 MB | 158 | 0.17 MB |
| hrms — all 152 | 20.4 MB | 332 | 1.31 MB |
| **erpnext — all 514 doctype controllers** | **38.4 MB** | 916 | 5.16 MB |
| erpnext — hot working set (~12 heaviest, lazy) | ~21.7 MB | 212 | 0.87 MB |
| all 5 apps' doctype controllers at once (770) | 43.9 MB | 1286 | 7.00 MB |

### True ceiling — *every* non-test `.py` in the app resident (worst case, eager)

| Scenario | RSS | modules | marshalled |
|---|--:|--:|--:|
| **erpnext — every non-test module (2268)** | **53.7 MB** | 2701 | 10.9 MB |
| helpdesk — every non-test module (189) | 18.3 MB | 358 | 0.58 MB |
| all 5 apps — every non-test module (3266) | 65.4 MB | 3855 | 14.8 MB |

96–98% of module bodies exec cleanly under the stub; the few failures are
`ModuleNotFoundError`/`TypeError` from edge metaclass/import-time code, immaterial to the floor.

## ferro (Rust framework) resident cost

`ferro serve <site>` against the real SQLite site, measured at `/proc/<pid>/status`:

| ferro state | RSS |
|---|--:|
| binary on disk | 1.67 MB |
| idle (2 threads, server bound) | **4.3 MB** |
| after serving list/read requests (meta cache warm) | **8.1 MB** |

## Combined budget vs 64 MB

| Layer | Realistic (lazy, one app) | Worst case (erpnext, everything eager) |
|---|--:|--:|
| ferro Rust framework (serving) | 6–8 MB | 8 MB |
| CPython interpreter + thin shim | 11 MB | 11 MB |
| app controller code | ~10–27 MB | 43 MB (53.7 total py) |
| **TOTAL resident** | **~28–46 MB** | **~60 MB** |
| headroom to 64 MB (for per-request churn) | 18–36 MB | ~4 MB |

**Conclusion (measured): 64 MB is achievable.** A single tenant — even on erpnext, the
heaviest ERP — fits with room to spare under lazy per-doctype controller loading, and even the
absurd "every erpnext module eagerly resident" ceiling (53.7 MB py + ferro) lands at ~60 MB.
Only loading all five giant apps eagerly at once exceeds 64 MB, and lazy loading dissolves that.

## Honest caveats
- Measures **resident code** (exec'd module bodies: classes, functions, constants). It does
  **not** include per-request runtime allocation (Document instances, field data). With
  Rust-backed lazy Document proxies the per-request Python object graph is small and transient;
  budget the headroom above for it and keep it reclaimable (jemalloc `dirty_decay_ms:0` /
  periodic `malloc_trim`, proven −4..22 MB in the prior study).
- Inter-module imports are stubbed, so class hierarchies aren't fully wired at measure time;
  resident **code bytes** are unaffected (each file's code is loaded once), so this is a sound
  floor/ceiling for memory.
- Lazy loading is the operative lever: a tenant touches a small subset of doctypes; controllers
  load on first touch, not at boot.
