# 00 — Methodology: running Frappe's test suite against ferro

## The core problem
Frappe's test suite is written to run **in-process**: `FrappeAPITestCase` builds a werkzeug WSGI
test client over `frappe.app.application` (`frappe.utils.get_test_client()`), and the vast majority
of tests call `frappe.*` Python APIs directly. ferro is **not a Python module** — it is a separate
Rust server. The only way ferro is exercised is **over HTTP**. So "run the suite against ferro"
splits into three distinct, separately-valid activities:

1. **HTTP differential** (empirical, the hard signal): take the test files that actually make HTTP
   calls and run them against a live **ferro** server AND a live **CPython Frappe** server (the
   *oracle*), with the SAME harness, and diff per-test. A test that passes on the oracle but fails
   on ferro is a real ferro shortfall. A test that fails on BOTH is an environment/harness artifact,
   not a ferro bug. See `01-http-differential-results.md`.
2. **Behavioral fidelity** (the bulk): most tests are in-process and never touch ferro, but their
   assertions *encode behaviors ferro reimplements in Rust* (ORM filters, naming, permissions, REST
   envelopes…). For those we compare the spec test to ferro's source behavior-by-behavior, with
   targeted HTTP probes where decidable. See `04-behavioral-findings.md`.
3. **Relevance classification** (breadth): for every one of the 276 test files, judge whether ferro
   should match it 1:1 / mostly / somewhat / shouldn't-care, given ferro's deliberately-narrow scope.
   See `03-test-class-judgments.md`.

## The differential harness
- **Shim** `ferro-test-harness/ferro_client.py`: a drop-in replacement for `get_test_client()` that
  speaks **real HTTP** to a target server, returning a werkzeug-`TestResponse`-compatible object
  (`.status_code/.json/.headers/.data/.text`). It is injected before test modules import
  (`run_one.py` / `run_one_json.py` patch `frappe.utils.get_test_client`).
- **Auth bridging**: Frappe's API tests authenticate by passing `sid` (a session id). ferro has no
  session auth, and even CPython needs a live session. The shim bridges: when a request carries
  `sid` (or `frappe.tests.test_api.authorization_token` is set), it attaches a **token** header
  instead. Critically it reads the Administrator api_key:secret **live from the committed DB** each
  call (Fernet-decrypting the secret) because `generate_admin_keys()` rotates+commits the key
  mid-suite — a statically captured token goes stale → spurious 401s.
- **Param translation**: werkzeug passes GET params as a JSON *body*; the shim moves GET/DELETE
  params to the query string (where a real client/ferro reads them) and POST/PUT/PATCH params to a
  JSON body. `sid` is stripped (it is auth, replaced by the token header).
- **Symmetry**: the identical shim runs against both servers, so differences are attributable to the
  server, not the harness.

## The two servers
- **ferro**: `ferro serve <site> --desk --default-user Guest -b 127.0.0.1:8081 --threads 4`
  (`--desk` exposes the broad `frappe.client.*`/`frappe.desk.*` method surface; `--default-user
  Guest` keeps correct unauthenticated semantics, since `--desk` alone would default to Administrator).
- **oracle**: `bench serve --port 8002 --noreload` (CPython werkzeug dev server, threaded).

## The benches (oracle DB protected)
The real bench `bench-cpython314` (SQLite site `mysite.sqlite`, **only the `frappe` app installed**)
is the ground truth and was snapshotted byte-for-byte to
`/home/frappe/.test-oracle-snapshots/oracle_pristine.db` before anything ran. Disposable copies:
- `bench-test`  → served by **ferro** (8081)
- `bench-oracle`→ served by **CPython** (8002)
- `bench-base`  → **in-process** CPython baseline (no shim), full core suite
Both servers' DBs were reset to the pristine snapshot and given Frappe-style (Fernet-encrypted)
Administrator API keys (`gen_keys.py`) so both accept token auth identically.

## What this harness CANNOT test (documented limitations)
- **Server-side writes**: Frappe's `IntegrationTestCase` holds a SQLite write lock for the duration
  of each test (per-test transaction/rollback isolation). When the test then asks the *server*
  (a separate process) to write, the server hits `SQLITE_BUSY` after the 5s busy-timeout →
  `DatabaseError` 500 — on **both** ferro and the oracle. So create/update/delete/copy/bulk-update
  tests are inherently un-runnable through this dual-process harness (correctly classified
  BOTH_BAD). ferro's write path is instead validated by `measurements/verify.py` and by direct
  standalone probes (a standalone POST create returns 200). This is a property of SQLite + the test
  framework, not of ferro.
- **Cookie/session clients**: ferro has no session store, so any client that logs in and then relies
  on the `sid` cookie (FrappeClient password login, browser non-admin Desk) cannot authenticate
  subsequent calls. This is itself a finding (see `05-discovered-behaviors.md`), but it also means
  FrappeClient-based tests are largely masked.

## Reproduce
```
# servers
ferro serve /home/frappe/benches/bench-test/sites/mysite.sqlite --desk --default-user Guest -b 127.0.0.1:8081
(cd /home/frappe/benches/bench-oracle/sites && ../env/bin/python -m frappe.utils.bench_helper frappe --site mysite.sqlite serve --port 8002 --noreload)
# differential
ferro-test-harness/driver_json.sh ferro  http://127.0.0.1:8081 <db> <enckey> <sites> <modules...>
ferro-test-harness/driver_json.sh oracle http://127.0.0.1:8002 <db> <enckey> <sites> <modules...>
ferro-test-harness/diff.py            # -> results/DIFF.md
# in-process baseline
ferro-test-harness/driver_baseline.sh # -> results/baseline/*.json
```
