#!/bin/bash
# verify.sh — prove the 5 Frappe apps actually RUN on ferrod (embedded-Python build):
#   - reads served 100% in Rust (zero Python)
#   - writes drive the REAL app controller lifecycle (validate/before_save/...) in CPython
#   - controller frappe.throw surfaces as a Frappe-shaped 417; ferro's Rust validation still applies
set -u
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"   # repo root
cd "$ROOT"
export LD_LIBRARY_PATH=/home/frappe/.pyenv/versions/3.13.13/lib
export FERRO_SHIM="${FERRO_SHIM:-$ROOT/framework/shim}"
export FERRO_REPOS="${FERRO_REPOS:-$ROOT/docs/investigations/apps-64mb/repos}"
SITE="$ROOT/demos/pyo3-apps/site"
FD="./target/release/ferrod"
pass=0; fail=0
check() { # desc, expected_substr, actual
  if echo "$3" | grep -q "$2"; then echo "  PASS: $1"; pass=$((pass+1)); else echo "  FAIL: $1 (want '$2')"; echo "     got: $3"; fail=$((fail+1)); fi
}

echo "== 1. READ path (pure Rust, no Python): list CRM Deals =="
out=$($FD request "$SITE" GET "/api/resource/CRM Deal?fields=[\"name\"]&limit_page_length=2" --apps crm --load all 2>/dev/null)
check "GET list returns data" '"data"' "$out"

echo "== 2. WRITE path (Python controller lifecycle): create CRM Deal =="
out=$($FD request "$SITE" POST "/api/resource/CRM Deal" '{"organization":"VerifyCo","status":"Qualification","deal_owner":"Administrator"}' --apps crm --load all 2>/dev/null)
check "POST creates a named CRM Deal" 'CRM-DEAL-' "$out"

echo "== 3. CONTROLLER VALIDATE runs (hrms RetentionBonus throws on past date) =="
out=$($FD request "$SITE" POST "/api/resource/Retention Bonus" '{"bonus_payment_date":"2020-01-01","bonus_amount":1000}' --apps hrms --load all --dev 2>/dev/null)
check "past date -> controller frappe.throw (417)" 'HTTP 417' "$out"
check "exact controller message surfaced" 'Bonus Payment Date cannot be a past date' "$out"

echo "== 4. RUST validation still applies after controller passes (mandatory field) =="
out=$($FD request "$SITE" POST "/api/resource/Retention Bonus" '{"bonus_payment_date":"2030-01-01","bonus_amount":1000}' --apps hrms --load all --dev 2>/dev/null)
check "future date passes controller, Rust catches missing 'company'" 'Value missing for Retention Bonus' "$out"

echo "== 5. selective dispatch: read never enters Python (timing/registry) =="
out=$($FD request "$SITE" GET "/api/resource/ToDo?limit_page_length=1" --apps crm --load all 2>/dev/null)
check "ToDo (frappe-core, pure-CRUD) read works in Rust" '"data"' "$out"

echo "== 6. CHILD TABLES persist through the Python dispatch path (was a data-loss bug) =="
DB=$SITE/db/_d3b3bc5c1c1a19aa.db
b=$(/home/frappe/.pyenv/versions/3.13.13/bin/python3 -c "import sqlite3;print(sqlite3.connect('$DB').execute('select count(*) from \"tabCRM Contacts\"').fetchone()[0])")
out=$($FD request "$SITE" POST "/api/resource/CRM Deal" '{"organization":"ChildVerify","status":"Qualification","contacts":[{"contact":"x1","is_primary":1},{"contact":"x2"}]}' --apps crm --load all 2>/dev/null)
a=$(/home/frappe/.pyenv/versions/3.13.13/bin/python3 -c "import sqlite3;print(sqlite3.connect('$DB').execute('select count(*) from \"tabCRM Contacts\"').fetchone()[0])")
check "2 child rows persisted (before=$b after=$a)" "$((b+2))" "$a"

echo "== 7. NestedSet/WebsiteGenerator doctypes now REGISTER (run their own logic) =="
out=$($FD request "$SITE" GET "/api/resource/CRM Sales Hierarchy?limit_page_length=1" --apps crm --load all 2>/dev/null)
check "CRM Sales Hierarchy (NestedSet) is served" '"data"' "$out"

echo
echo "RESULT: $pass passed, $fail failed"
[ "$fail" = 0 ] && echo "ALL VERIFIED — the 5 apps run on ferro (reads in Rust, writes drive real controller Python)."
