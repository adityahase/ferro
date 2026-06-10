# 07 — In-process baseline & app-level testing

## In-process CPython baseline (the 83-file core suite)
To distinguish "ferro can't" from "this environment can't", the full `frappe/tests/` core suite was
run **in-process** on plain CPython/SQLite (no ferro, no shim) on `bench-base`.

```
TOTAL core-suite tests: 502
pass=339  skip=29  fail=26  error=108     → pass+skip = 368 (73%)
```

**Even Frappe-on-itself only reaches ~73% here.** The 134 non-passing are dominated by environment,
not logic:
- `test_db_query` (45 err / 50): test-record fixture conflicts (`UNIQUE … tabTest Blogger.short_name`)
  — leftover data from sequential module runs against one DB; a data-isolation artifact, not a query
  bug. (db_query is still read as the filter *spec* in `04-behavioral-findings.md`.)
- `test_db` (9 fail / 5 err / 26 skip): SQLite-specific — e.g. `ImplicitCommitError` on
  `ALTER TABLE … RENAME` inside a transaction; many tests `@skip` on non-MariaDB.
- `test_email` (14 err): no SMTP / email account setup.
- `test_frappe_client` (13 err): admin password not set to the test default on the pristine DB.
- `test_auth` (9 err), `test_global_search` (6 err), `test_background_jobs` (3 err),
  `test_db_update` (5 fail): SQLite-isms / Redis / worker / setup.

**Implication.** Attributing failures to ferro from a single run is unsound — the baseline is noisy.
The trustworthy signal is the symmetric **HTTP differential** (`01-…`), where ferro and the CPython
oracle run the identical harness in the identical environment, so only the server differs.

Full per-module numbers: `ferro-test-harness/results/baseline/*.json`.

## App-level testing (erpnext / crm / helpdesk / gameplan)
The request was: "if most tests pass, run tests for individual apps as well." Two facts make
app-level *test execution* inapplicable here:

1. **No app but `frappe` is installed.** All four benches have `apps.txt = ["frappe"]`. erpnext and
   crm source exist only as unbuilt clones under
   `/home/frappe/.absorbed-siblings/ferro-apps-investigation/repos/{erpnext,crm}` — not installed,
   not schema-synced into any site. Installing erpnext (hundreds of doctypes, MariaDB-oriented
   patches) into a SQLite site to run its suite is a large, separate effort and out of scope for a
   compatibility read of ferro.
2. **App test suites are in-process Python controller tests.** erpnext/crm tests instantiate
   controllers and assert on Python business logic (`frappe.get_doc(...).submit()`,
   GL entries, validations). They never go through HTTP, so they would exercise **CPython Frappe**,
   not ferro — running them tells you nothing about ferro. The behaviors they need at the ferro layer
   are the same CRUD/ORM/perm/naming surface already audited here; the controller logic itself is the
   `ferrod` (Python-fallthrough) tier's responsibility, not pure ferro's.

**The real app-compatibility contract is the v2 REST API.** The frappe-ui SPAs (CRM, Helpdesk,
Gameplan) talk to the backend over `/api/v2/*` and `/api/method/<app>.*`. So app compatibility for
pure ferro reduces to: (a) the v2 document/method surface (audited in `01`/`04` — the v2 envelope and
CRUD largely match; **v2 bulk ops are the notable gap**, G4), and (b) the set of `@frappe.whitelist()`
app methods the frontends call, which pure ferro answers with 404 unless ported (D6) — by design the
`ferrod` tier handles these. Per project memory, the SPAs *do* render against ferro today because it
runs the frontends with `default_user=Administrator` and falls through app methods to Python in the
`ferrod` build.

**Recommendation.** Treat "app compatibility" as a v2-API conformance target, not a Python-suite
target. If you want empirical app coverage, the highest-value next step is to capture the actual
request stream a frappe-ui app issues (CRM list/detail/save) and replay it against pure ferro,
recording which `/api/method/*` calls 404 — that enumerates exactly which app methods would need
porting vs. left to `ferrod`.
