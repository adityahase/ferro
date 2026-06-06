#!/usr/bin/env python3
"""Adversarial robustness probe: SQL-injection attempts, malformed input, weird URLs.
A request-triggerable panic would crash the whole server (panic=abort), so the bar is:
every probe returns a clean HTTP status and the server/CLI never aborts."""
import json, re, subprocess, sys
SITE = "/home/frappe/benches/bench-cpython314/sites/mysite.sqlite"
FERRO = "/home/frappe/ferro/target/release/ferro"

def req(method, url, body=None, user="Administrator"):
    args = [FERRO, "request", SITE, method, url]
    if body is not None: args.append(body)
    args += ["--user", user]
    p = subprocess.run(args, capture_output=True, text=True, timeout=30)
    if p.returncode != 0 and not p.stdout:
        return ("ABORT", p.returncode, p.stderr[:200])
    m = re.match(r"HTTP (\d+)", p.stdout)
    return (int(m.group(1)) if m else "NOPARSE", p.stdout[:160])

PROBES = [
    # SQL injection via filter field name (must be rejected as unknown field, not executed)
    ("GET", '/api/resource/User?filters=[["name; DROP TABLE tabUser--","=","x"]]'),
    ("GET", '/api/resource/User?fields=["name); DROP TABLE tabUser--"]'),
    ("GET", '/api/resource/User?order_by=name); DROP TABLE tabUser--'),
    # injection via doctype/table name
    ("GET", '/api/resource/User%22%3B%20DROP%20TABLE%20tabUser--/x'),
    # filter value with quotes (should be a bound param -> harmless)
    ("GET", '/api/resource/User?filters=[["name","=","x\' OR \'1\'=\'1"]]'),
    # malformed JSON in query params
    ("GET", '/api/resource/User?filters=not-json&fields=also-not-json'),
    ("GET", '/api/resource/User?filters=[[[[[[['),
    # malformed body
    ("POST", '/api/resource/ToDo', '{"description": '),
    ("POST", '/api/resource/ToDo', '[]'),
    ("POST", '/api/resource/ToDo', 'null'),
    # weird URLs
    ("GET", '/'),
    ("GET", '/api'),
    ("GET", '/api/resource'),
    ("GET", '/api/resource/'),
    ("GET", '/api/method'),
    ("GET", '/api/wat/x'),
    ("GET", '/api/resource/User/%2e%2e%2f%2e%2e'),
    ("GET", '/api/resource/User?limit_page_length=-5'),
    ("GET", '/api/resource/User?limit_page_length=abc'),
    ("GET", '/api/resource/User?limit_start=-1'),
    ("DELETE", '/api/resource/ToDo/does-not-exist-xyz'),
    ("PUT", '/api/resource/ToDo/does-not-exist-xyz', '{"status":"Open"}'),
    ("GET", '/api/resource/User/' + ("A"*5000)),  # very long name
    ("GET", '/api/resource/User?filters=' + json.dumps([["enabled","in",[]]])),  # empty IN
    ("GET", '/api/resource/User?filters=' + json.dumps([["enabled","between",[1]]])),  # bad between
]

aborts = 0
print("=== adversarial probe ===")
for p in PROBES:
    r = req(*p)
    flag = ""
    if r[0] == "ABORT":
        aborts += 1; flag = "  <<< ABORT/PANIC"
    print(f"  {p[0]:6} {p[1][:70]:70} -> {r[0]}{flag}")

# verify table still exists after injection attempts
import sqlite3
DB = SITE + "/db/_d3b3bc5c1c1a19aa.db"
con = sqlite3.connect(DB)
n = con.execute("SELECT COUNT(*) FROM tabUser").fetchone()[0]
con.close()
print(f"\ntabUser still present with {n} rows (injection did not drop it)")
print(f"\n=== {'PASS — no aborts' if aborts==0 and n>0 else 'FAIL'} ({aborts} aborts) ===")
sys.exit(1 if aborts or n==0 else 0)
