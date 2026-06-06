# ferro — a Rust runtime replacing the CPython+Frappe REST worker

**Goal:** replace the Python interpreter + Frappe framework with a Rust runtime and get the
per-worker memory footprint **under 64 MB**. **Result: met by a wide margin** — idle ~4.6 MB,
~17.7 MB peak under heavy concurrent load at the default thread count, vs a ~115 MB warm
CPython+Frappe worker (≈85% reduction). Binary: 1.7 MB, statically minimal, **zero crypto/web
framework dependencies beyond `rusqlite`, `tiny_http`, `serde_json`**.

## What it is

`ferro` serves the Frappe **v1 REST API** (`/api/resource/...`, `/api/method/{ping,get_logged_user}`)
directly against the **same** SQLite site database a Frappe site already uses (`tab<DocType>`,
`tabSingles`, `tabSeries`, `__Auth`, `tabDocPerm`/`tabCustom DocPerm`, `tabHas Role`). No Python,
no Frappe import graph, no per-worker object graph. A client (frappe-js-sdk, FrappeClient, curl)
talks to ferro exactly as it would to a Frappe gunicorn worker for CRUD + auth.

```
ferro serve <site-dir-or-db> [--port N] [--threads N] [--default-user U] [--meta-cap N] [--dev]
ferro request <site-dir-or-db> <METHOD> <url> [body] [--token k:s] [--user U]   # in-process, for tests
ferro provision-key <site-dir-or-db> <user>
```

Source (≈2,600 lines Rust): `src/main.rs` (routing/envelope/DoS guards), `src/orm.rs`
(list/get/insert/update/delete), `src/meta.rs` (LRU metadata cache), `src/auth.rs`
(token/Fernet/permissions), `src/naming.rs` (autoname rules), `src/crypto.rs` (Fernet:
AES-128-CBC + HMAC-SHA256 + base64, dependency-free), `src/util.rs`.

## Memory (measured on-host, smaps_rollup, peak under load)

Load = all 278 doctype metas cached + 2,224 list/meta requests + 200 concurrent CRUD cycles.

| Config              | idle RSS | peak RSS | peak USS |
|---------------------|---------:|---------:|---------:|
| 1 thread (idle)     |  4.6 MB  |    —     |  0.9 MB  |
| 2 threads           |    —     | 12.0 MB  |  9.7 MB  |
| **4 threads (default)** | ~5 MB | **17.7 MB** | **15.2 MB** |
| 8 threads           |    —     | 28.7 MB  | 26.3 MB  |
| 16 threads          |    —     | 46.2 MB  | 43.7 MB  |

Per-thread cost ≈2.5 MB (own SQLite connection page cache, capped at 2 MiB, + glibc arena).
Bounded LRU meta cache (`--meta-cap`) keeps resident metadata flat under doctype churn.
**Every configuration is under the 64 MB target;** the default is ~3.6× under it and ~6.5×
lighter than the CPython worker it replaces.

For comparison, the prior investigation (see `/home/frappe/runtime-memory-investigation/`)
found the CPython worker's interpreter is only ~3.4 MB USS — the other ~100 MB is Frappe's own
import + object graph, which a runtime swap *within Python* could never reclaim. ferro reclaims
it by not having it.

## Fidelity: audited against real Frappe, then fixed

A multi-agent audit diffed ferro against the actual Frappe 17.0.0-dev source across 9 REST
dimensions and adversarially verified every finding: **57 confirmed divergences** (4 critical,
11 high, 21 medium, 21 low). The critical/high set and most mediums are now fixed and verified
by a 41-assertion functional suite (`measurements/verify.py`, all green) plus a 25-probe
adversarial robustness test (`measurements/probe.py`, 0 panics, all injection attempts rejected).

### Fixed & verified

- **Crash/OOM:** `random_name()` read `/dev/urandom` to EOF (never-ending) → unbounded alloc →
  SIGKILL; broke every write path. Now reads a fixed 5 bytes.
- **Auth:** Fernet-decrypts real Frappe `encrypted=1` api_secrets with the site `encryption_key`
  (verified end-to-end against a Python-`cryptography`-encrypted secret); plaintext still works;
  HTTP **401** on bad/unknown credentials (no silent Guest downgrade); Basic `key:secret` accepted.
- **Permissions:** `if_owner` row scoping on list (owner filter) and on get/update/delete
  (owner check); **Custom DocPerm** override; **permlevel** field masking on reads (a non-admin
  with permlevel-0 read on User no longer receives `api_key`/`api_secret`/`roles`); Administrator
  bypass.
- **Naming:** `naming_series`/`.####`/`format:`/Expression with date parts (YY/MM/DD/YYYY/JJJ/WW/
  timestamp) and `{field}` substitution, backed by the atomic `tabSeries` counter (UPSERT…RETURNING
  — no collisions, stays in sync with CPython workers); `field:`, `hash`, `UUID`, `autoincrement`,
  case-insensitive `prompt`; Frappe `validate_name` rules.
- **List:** `limit_page_length=0` → unlimited (was 0 rows); `in`/`not in` comma-string splitting.
- **Insert/update/delete:** docfield defaults (incl. `__user`/`Today`/Select-first/Check casts);
  required-field validation; server-authoritative `owner`/`creation`/`modified*` (client cannot
  override); protected `docstatus`/`idx`/`parent*` on update; submitted (docstatus=1) docs blocked
  from delete; **all multi-statement writes wrapped in IMMEDIATE transactions**.
- **Singles:** numeric fieldtype casting (Check/Int/Float…); standard fields exposed.
- **Virtual doctypes:** list → `[]`, get → 404 (was a 500 DatabaseError).
- **Envelope:** `{"data": ...}` success; v1 error shape with correct `exc_type` + status
  (404/417/403/401/**409 duplicate**/413/500); `_server_messages` as JSON-array-of-JSON-objects
  (frappe-js-sdk compatible); `exception`/raw DB text only in `--dev`; DELETE → `{"data":"ok"}` @202.
- **Routing/safety:** form-urlencoded bodies + the FrappeClient `data` field + query-param merge;
  `<path:name>` (slash-containing names); 8 MiB body cap (413); SQL-injection-safe (every
  identifier validated against meta columns or bound; verified by probe); LRU (not FIFO) meta cache.

See `measurements/CONFIRMED-FINDINGS.md` and the audit output for the full list and Frappe refs.
Remaining (intentionally-deferred) divergences are documented in `LIMITATIONS.md`.
