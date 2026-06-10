#!/usr/bin/env python3
"""Functional verification of ferro against expected Frappe REST semantics.

Uses the in-process `ferro request` CLI (full route/auth/orm stack, no socket). Sets up DB
fixtures directly, runs assertions for every fix from the fidelity audit, then cleans up.
"""
import json, os, re, subprocess, sys, sqlite3, base64

SITE = "/home/frappe/benches/bench-cpython314/sites/mysite.sqlite"
DB = SITE + "/db/_d3b3bc5c1c1a19aa.db"
FERRO = "/home/frappe/ferro/target/release/ferro"
ENC_KEY = "nX12-lJmJ5bw5YmIpicLAZVuFBqbU9j9EtSLnepnSCI="  # from site_config.json

PASS = 0
FAIL = 0
def check(name, cond, detail=""):
    global PASS, FAIL
    if cond:
        PASS += 1
        print(f"  PASS  {name}")
    else:
        FAIL += 1
        print(f"  FAIL  {name}   {detail}")

def req(method, url, body=None, token=None, user=None, desk=False):
    args = [FERRO, "request", SITE, method, url]
    if body is not None:
        args.append(body if isinstance(body, str) else json.dumps(body))
    if token:
        args += ["--token", token]
    if user:
        args += ["--user", user]
    if desk:
        args.append("--desk")
    p = subprocess.run(args, capture_output=True, text=True, timeout=30)
    out = p.stdout
    m = re.match(r"HTTP (\d+)\n(.*)", out, re.S)
    if not m:
        return -1, {"raw": out, "err": p.stderr}
    status = int(m.group(1))
    try:
        return status, json.loads(m.group(2))
    except Exception:
        return status, {"raw": m.group(2)}

# ----------------------------- fixtures -----------------------------
def setup():
    con = sqlite3.connect(DB); cur = con.cursor()
    def ex(s, *a):
        try: cur.execute(s, a)
        except Exception as e: print("  setup warn:", e)
    # role
    ex("INSERT OR IGNORE INTO tabRole (name, creation, modified, owner, modified_by, role_name, disabled) "
       "VALUES ('Ferro Test', datetime('now'), datetime('now'), 'Administrator','Administrator','Ferro Test',0)")
    # users
    for u in ("ferro_test@example.com", "ferro_enc@example.com"):
        ex("INSERT OR IGNORE INTO tabUser (name, creation, modified, owner, modified_by, email, first_name, enabled, user_type) "
           "VALUES (?, datetime('now'), datetime('now'),'Administrator','Administrator', ?, 'Ferro', 1, 'System User')", u, u)
    # roles for ferro_test
    ex("DELETE FROM \"tabHas Role\" WHERE parent='ferro_test@example.com'")
    ex("INSERT INTO \"tabHas Role\" (name, creation, modified, owner, modified_by, parent, parenttype, parentfield, role) "
       "VALUES ('frt-role-1', datetime('now'), datetime('now'),'Administrator','Administrator','ferro_test@example.com','User','roles','Ferro Test')")
    # plaintext token for ferro_test
    ex("UPDATE tabUser SET api_key='ferrotestkey' WHERE name='ferro_test@example.com'")
    ex("DELETE FROM __Auth WHERE doctype='User' AND name='ferro_test@example.com' AND fieldname='api_secret'")
    ex("INSERT INTO __Auth (doctype,name,fieldname,password,encrypted) VALUES ('User','ferro_test@example.com','api_secret','ferrotestsecret',0)")
    # Fernet-encrypted token for ferro_enc (real Frappe at-rest model)
    from cryptography.fernet import Fernet
    enc = Fernet(ENC_KEY.encode()).encrypt(b"enc-secret-value").decode()
    ex("UPDATE tabUser SET api_key='ferroenckey' WHERE name='ferro_enc@example.com'")
    ex("DELETE FROM __Auth WHERE doctype='User' AND name='ferro_enc@example.com' AND fieldname='api_secret'")
    ex("INSERT INTO __Auth (doctype,name,fieldname,password,encrypted) VALUES ('User','ferro_enc@example.com','api_secret',?,1)", enc)
    # Custom DocPerm: Note read+create if_owner for Ferro Test (overrides tabDocPerm -> if_owner-only)
    ex("DELETE FROM \"tabCustom DocPerm\" WHERE parent IN ('Note','User') AND role='Ferro Test'")
    ex("INSERT INTO \"tabCustom DocPerm\" (name,creation,modified,owner,modified_by,parent,role,permlevel,\"read\",\"write\",\"create\",\"delete\",if_owner) "
       "VALUES ('frt-cdp-note',datetime('now'),datetime('now'),'Administrator','Administrator','Note','Ferro Test',0,1,0,1,0,1)")
    # Custom DocPerm: User read permlevel 0 for Ferro Test (to test permlevel masking)
    ex("INSERT INTO \"tabCustom DocPerm\" (name,creation,modified,owner,modified_by,parent,role,permlevel,\"read\") "
       "VALUES ('frt-cdp-user',datetime('now'),datetime('now'),'Administrator','Administrator','User','Ferro Test',0,1)")
    # B-MET-1: a permlevel-1 Custom Field on ToDo + its physical column (ADD COLUMN is idempotent-
    # guarded by the try/except; cleanup drops it).
    ex("ALTER TABLE tabToDo ADD COLUMN ferro_cf TEXT")
    ex("DELETE FROM \"tabCustom Field\" WHERE name='frt-cf-1'")
    ex("INSERT INTO \"tabCustom Field\" (name,creation,modified,owner,modified_by,dt,fieldname,label,fieldtype,permlevel,idx) "
       "VALUES ('frt-cf-1',datetime('now'),datetime('now'),'Administrator','Administrator','ToDo','ferro_cf','Ferro CF','Data',1,99)")
    # B-MET-2: a Property Setter raising User.birth_date (permlevel 0) to permlevel 1.
    ex("DELETE FROM \"tabProperty Setter\" WHERE name='frt-ps-1'")
    ex("INSERT INTO \"tabProperty Setter\" (name,creation,modified,owner,modified_by,doc_type,doctype_or_field,field_name,property,property_type,value) "
       "VALUES ('frt-ps-1',datetime('now'),datetime('now'),'Administrator','Administrator','User','DocField','birth_date','permlevel','Int','1')")
    # B-DOC-2: make ToDo.priority set_only_once via a Property Setter.
    ex("DELETE FROM \"tabProperty Setter\" WHERE name='frt-soo'")
    ex("INSERT INTO \"tabProperty Setter\" (name,creation,modified,owner,modified_by,doc_type,doctype_or_field,field_name,property,property_type,value) "
       "VALUES ('frt-soo',datetime('now'),datetime('now'),'Administrator','Administrator','ToDo','DocField','priority','set_only_once','Check','1')")
    con.commit(); con.close()

def cleanup():
    con = sqlite3.connect(DB); cur = con.cursor()
    for s in [
        "DELETE FROM \"tabHas Role\" WHERE parent='ferro_test@example.com'",
        "DELETE FROM __Auth WHERE name IN ('ferro_test@example.com','ferro_enc@example.com') AND fieldname='api_secret'",
        "DELETE FROM \"tabCustom DocPerm\" WHERE role='Ferro Test'",
        "DELETE FROM tabNote WHERE title LIKE 'ferro-note-%'",
        "DELETE FROM tabUser WHERE name IN ('ferro_test@example.com','ferro_enc@example.com')",
        "DELETE FROM tabRole WHERE name='Ferro Test'",
        "DELETE FROM tabToDo WHERE description LIKE 'ferro-verify-%'",
        "DELETE FROM \"tabConsole Log\" WHERE type='y'",
        "DELETE FROM \"tabCustom Field\" WHERE name='frt-cf-1'",
        "DELETE FROM \"tabProperty Setter\" WHERE name IN ('frt-ps-1','frt-soo')",
        "ALTER TABLE tabToDo DROP COLUMN ferro_cf",
        "DELETE FROM tabToDo WHERE description LIKE 'ferro-link-%' OR description LIKE 'ferro-olock-%' OR description LIKE 'ferro-soo-%'",
    ]:
        try: cur.execute(s)
        except Exception as e: print("  cleanup warn:", e)
    con.commit(); con.close()

# ----------------------------- tests -----------------------------
def run_tests():
    ADMIN = "Administrator"
    TTOK = "ferrotestkey:ferrotestsecret"
    ENCTOK = "ferroenckey:enc-secret-value"

    print("\n[auth]")
    s,b = req("GET","/api/method/frappe.auth.get_logged_user", token=TTOK)
    check("plaintext token authenticates", s==200 and b.get("message")=="ferro_test@example.com", f"{s} {b}")
    s,b = req("GET","/api/method/frappe.auth.get_logged_user", token=ENCTOK)
    check("FERNET-encrypted token authenticates (real Frappe at-rest)", s==200 and b.get("message")=="ferro_enc@example.com", f"{s} {b}")
    s,b = req("GET","/api/method/frappe.auth.get_logged_user", token="ferrotestkey:WRONGSECRET")
    check("bad credentials -> 401 (not silent Guest)", s==401, f"{s} {b}")
    s,b = req("GET","/api/method/frappe.auth.get_logged_user", token="ferroenckey:wrong")
    check("wrong fernet secret -> 401", s==401, f"{s} {b}")

    print("\n[list query]")
    s,b = req("GET",'/api/resource/DocType?fields=["name"]&limit_page_length=5', user=ADMIN)
    n5 = len(b.get("data",[]))
    check("limit_page_length=5 returns 5", s==200 and n5==5, f"{s} n={n5}")
    s,b = req("GET",'/api/resource/DocType?fields=["name"]&limit_page_length=0', user=ADMIN)
    n0 = len(b.get("data",[]))
    check("limit_page_length=0 means UNLIMITED (>200 doctypes)", s==200 and n0>200, f"{s} n={n0}")
    s,b = req("GET",'/api/resource/DocType?fields=["name"]&filters={"issingle":1}&limit_page_length=3', user=ADMIN)
    check("dict filter works", s==200 and len(b.get("data",[]))==3, f"{s} {b}")
    s,b = req("GET",'/api/resource/User?fields=["name"]&filters=[["name","in","Administrator,Guest"]]', user=ADMIN)
    names = sorted(d["name"] for d in b.get("data",[]))
    check("'in' with comma-separated string splits", s==200 and names==["Administrator","Guest"], f"{s} {names}")

    print("\n[role resolution: Guest is not 'All' (FIX-1)]")
    # ToDo grants read to role "All". Before FIX-1 Guest inherited "All" and could read it.
    s,b = req("GET", '/api/resource/ToDo?limit_page_length=1', user="Guest")
    check("Guest GET /api/resource/ToDo -> 403 (no 'All' role)", s==403 and b.get("exc_type")=="PermissionError", f"{s} {b}")
    s,b = req("GET", '/api/resource/ToDo/anything', user="Guest")
    check("Guest GET /api/resource/ToDo/<name> -> 403", s==403, f"{s} {b}")
    # A normal System User still inherits "All", so it CAN read an All-granted doctype.
    s,b = req("GET", '/api/resource/ToDo?limit_page_length=1', token=TTOK)
    check("System user still inherits 'All' (reads ToDo)", s==200, f"{s} {b}")

    print("\n[frappe.client.* permission gate (FIX-9)]")
    # The desk method path must enforce the SAME read gate as /api/resource. Before FIX-9 Guest
    # could read User emails via frappe.client.get_list/get_value/get_count (perm bypass).
    s,b = req("GET", '/api/method/frappe.client.get_list?doctype=User&fields=["name","email"]', user="Guest", desk=True)
    check("Guest frappe.client.get_list User -> 403 (no leak)", s==403, f"{s} {b}")
    s,b = req("GET", '/api/method/frappe.client.get_value?doctype=User&fieldname=email&filters={}', user="Guest", desk=True)
    check("Guest frappe.client.get_value User.email -> 403", s==403, f"{s} {b}")
    s,b = req("GET", '/api/method/frappe.client.get_count?doctype=User', user="Guest", desk=True)
    check("Guest frappe.client.get_count User -> 403", s==403, f"{s} {b}")
    # Admin (and the Desk it powers) still works through the same path.
    s,b = req("GET", '/api/method/frappe.client.get_list?doctype=User&fields=["name"]&limit_page_length=2', user="Administrator", desk=True)
    check("Admin frappe.client.get_list User -> 200 (desk preserved)", s==200 and isinstance(b.get("message"), list), f"{s} {b}")
    # permlevel masking + null-stripping holds through the desk path too.
    s,b = req("GET", '/api/method/frappe.client.get?doctype=User&name=Administrator', token=TTOK, desk=True)
    d = b.get("message", {}) if isinstance(b.get("message"), dict) else {}
    check("client.get permlevel0 user reads User -> 200", s==200, f"{s} {b}")
    check("client.get does NOT leak api_key (permlevel 1)", "api_key" not in d, f"keys={list(d)[:8]}")
    check("client.get strips null-valued keys (no_nulls=True)", all(v is not None for v in d.values()), f"nulls={[k for k,v in d.items() if v is None]}")

    print("\n[filter shapes (B-FIL-1/2/3)]")
    s,b = req("GET", '/api/resource/User?fields=["name"]&filters=[["name","in","[\\"Administrator\\",\\"Guest\\"]"]]', user=ADMIN)
    names = sorted(d["name"] for d in b.get("data",[]))
    check("'in' with JSON-encoded list string parses (B-FIL-3)", s==200 and names==["Administrator","Guest"], f"{s} {names}")
    s,b = req("GET", '/api/resource/User?fields=["name"]&filters=[["name","not in",[null]]]&limit_page_length=0', user=ADMIN)
    check("'not in [null]' matches all rows (B-FIL-2)", s==200 and len(b.get("data",[]))>=2, f"{s} n={len(b.get('data',[]))}")
    s,b = req("GET", '/api/resource/User?fields=["name"]&filters=[["name","in",[null]]]', user=ADMIN)
    check("'in [null]' matches no rows (B-FIL-2)", s==200 and len(b.get("data",[]))==0, f"{s} n={len(b.get('data',[]))}")
    s,b = req("GET", '/api/resource/DocType?fields=["name"]&filters=[{"issingle":1}]&limit_page_length=3', user=ADMIN)
    check("dict element inside filters array (B-FIL-1)", s==200 and len(b.get("data",[]))==3, f"{s} {b}")
    s,b = req("GET", '/api/resource/User?fields=["name"]&or_filters=[{"name":"Administrator"},{"name":"Guest"}]&limit_page_length=0', user=ADMIN)
    names = sorted(d["name"] for d in b.get("data",[]))
    check("or_filters as list of dicts (B-FIL-1)", s==200 and names==["Administrator","Guest"], f"{s} {names}")

    print("\n[permlevel masking]")
    s,b = req("GET","/api/resource/User/Administrator", user=ADMIN)
    d = b.get("data",{})
    check("admin sees permlevel-1 field api_key", s==200 and "api_key" in d, f"{s} keys~{list(d)[:3]}")
    check("admin sees permlevel-1 child table 'roles'", "roles" in d, "")
    s,b = req("GET","/api/resource/User/Administrator", token=TTOK)
    d = b.get("data",{})
    check("non-admin (permlevel0 read) can read User", s==200, f"{s} {b}")
    check("non-admin does NOT see api_key (permlevel 1)", "api_key" not in d, f"got keys {list(d)[:8]}")
    check("non-admin does NOT see api_secret/roles (permlevel 1)", "api_secret" not in d and "roles" not in d, f"got {list(d)}")
    check("non-admin still sees permlevel-0 field email", "email" in d, f"got {list(d)[:8]}")

    print("\n[if_owner row scoping]")
    # create one Note owned by admin, one owned by ferro_test
    s,b = req("POST","/api/resource/Note", {"title":"ferro-note-admin"}, user=ADMIN)
    check("create Note as admin", s==200, f"{s} {b}")
    s,b = req("POST","/api/resource/Note", {"title":"ferro-note-owned"}, token=TTOK)
    check("create Note as ferro_test (if_owner create allowed)", s==200, f"{s} {b}")
    owned_name = b.get("data",{}).get("name")
    s,b = req("GET",'/api/resource/Note?fields=["name","owner"]&limit_page_length=0', token=TTOK)
    owners = set(d.get("owner") for d in b.get("data",[]))
    check("if_owner list shows ONLY own rows", s==200 and owners=={"ferro_test@example.com"}, f"{s} owners={owners}")
    s,b = req("GET",'/api/resource/Note?fields=["name","owner"]&limit_page_length=0', user=ADMIN)
    check("admin list shows all Notes", s==200 and len(b.get("data",[]))>=2, f"{s} n={len(b.get('data',[]))}")

    print("\n[insert: naming]")
    s,b = req("POST","/api/resource/Discussion Topic", {"title":"ferro topic"}, user=ADMIN)
    nm = b.get("data",{}).get("name","")
    check("naming_series TOPIC.#### -> TOPIC+digits", s==200 and re.fullmatch(r"TOPIC\d{4,}", nm) is not None, f"{s} name={nm!r} {b if s!=200 else ''}")
    s,b = req("POST","/api/resource/Event", {"subject":"ferro ev","starts_on":"2026-06-06 10:00:00"}, user=ADMIN)
    nm = b.get("data",{}).get("name","")
    check("naming_series EV.##### -> EV+digits", s==200 and re.fullmatch(r"EV\d{5,}", nm) is not None, f"{s} name={nm!r} {b if s!=200 else ''}")
    s,b = req("POST","/api/resource/Console Log", {"method":"x","type":"y"}, user=ADMIN)
    nm = b.get("data",{}).get("name","")
    check("format: 'Log on {timestamp}' substitutes", s==200 and nm.startswith("Log on "), f"{s} name={nm!r} {b if s!=200 else ''}")
    s,b = req("POST","/api/resource/Note", {"title":"ferro-note-hash"}, user=ADMIN)
    nm = b.get("data",{}).get("name","")
    check("hash autoname -> 10 hex", s==200 and re.fullmatch(r"[0-9a-f]{10}", nm) is not None, f"{s} name={nm!r}")

    print("\n[insert: validation + defaults + system fields]")
    s,b = req("POST","/api/resource/ToDo", {"status":"Open"}, user=ADMIN)
    check("reqd field 'description' missing -> 417", s==417 and b.get("exc_type")=="ValidationError", f"{s} {b}")
    s,b = req("POST","/api/resource/ToDo", {"description":"ferro-verify-1"}, user=ADMIN)
    todo = b.get("data",{})
    check("ToDo insert ok with description", s==200, f"{s} {b}")
    check("status default applied (Open)", todo.get("status")=="Open", f"status={todo.get('status')}")
    check("owner stamped = session user", todo.get("owner")==ADMIN, f"owner={todo.get('owner')}")
    todo_name = todo.get("name")
    # client cannot override owner
    s,b = req("POST","/api/resource/ToDo", {"description":"ferro-verify-2","owner":"evil@x.com"}, user=ADMIN)
    check("client-supplied owner is ignored (server overrides)", b.get("data",{}).get("owner")==ADMIN, f"owner={b.get('data',{}).get('owner')}")

    print("\n[update / delete]")
    s,b = req("PUT",f"/api/resource/ToDo/{todo_name}", {"status":"Closed"}, user=ADMIN)
    check("update status -> Closed", s==200 and b.get("data",{}).get("status")=="Closed", f"{s} {b}")
    s,b = req("PUT",f"/api/resource/ToDo/{todo_name}", {"docstatus":1}, user=ADMIN)
    check("protected field docstatus not writable via update", b.get("data",{}).get("docstatus")==0, f"docstatus={b.get('data',{}).get('docstatus')}")
    s,b = req("DELETE",f"/api/resource/ToDo/{todo_name}", user=ADMIN)
    check("DELETE returns 202 + {\"data\":\"ok\"}", s==202 and b.get("data")=="ok", f"{s} {b}")
    s,b = req("GET",f"/api/resource/ToDo/{todo_name}", user=ADMIN)
    check("deleted doc -> 404 DoesNotExistError", s==404 and b.get("exc_type")=="DoesNotExistError", f"{s} {b}")

    print("\n[document lifecycle: links / set_only_once / optimistic lock (B-DOC-1/2/3)]")
    # B-DOC-1: a non-empty Link to a missing target is rejected (LinkValidationError -> 417).
    s,b = req("POST","/api/resource/ToDo", {"description":"ferro-link-bad","allocated_to":"nobody@nowhere.invalid"}, user=ADMIN)
    check("Link to a missing target -> 417 (B-DOC-1)", s==417, f"{s} {b}")
    s,b = req("POST","/api/resource/ToDo", {"description":"ferro-link-ok","allocated_to":"Administrator"}, user=ADMIN)
    check("Link to an existing target -> 200 (B-DOC-1)", s==200, f"{s} {b}")
    if s==200: req("DELETE", f"/api/resource/ToDo/{b['data']['name']}", user=ADMIN)
    # B-DOC-3: optimistic concurrency on `modified`.
    s,b = req("POST","/api/resource/ToDo", {"description":"ferro-olock-1"}, user=ADMIN)
    ol_name = b.get("data",{}).get("name"); ol_mod = b.get("data",{}).get("modified")
    s,b = req("PUT", f"/api/resource/ToDo/{ol_name}", {"status":"Closed","modified":ol_mod}, user=ADMIN)
    check("update with current modified -> 200 (B-DOC-3)", s==200, f"{s} {b}")
    s,b = req("PUT", f"/api/resource/ToDo/{ol_name}", {"status":"Open","modified":"2000-01-01 00:00:00.000000"}, user=ADMIN)
    check("update with stale modified -> 417 TimestampMismatch (B-DOC-3)", s==417, f"{s} {b}")
    if ol_name: req("DELETE", f"/api/resource/ToDo/{ol_name}", user=ADMIN)
    # B-DOC-2: a set_only_once field (ToDo.priority via Property Setter) cannot change on update.
    s,b = req("POST","/api/resource/ToDo", {"description":"ferro-soo-1","priority":"Low"}, user=ADMIN)
    soo_name = b.get("data",{}).get("name")
    s,b = req("PUT", f"/api/resource/ToDo/{soo_name}", {"priority":"High"}, user=ADMIN)
    check("changing a set_only_once field -> 417 (B-DOC-2)", s==417, f"{s} {b}")
    s,b = req("PUT", f"/api/resource/ToDo/{soo_name}", {"status":"Closed"}, user=ADMIN)
    check("updating other fields with set_only_once unchanged -> 200 (B-DOC-2)", s==200, f"{s} {b}")
    if soo_name: req("DELETE", f"/api/resource/ToDo/{soo_name}", user=ADMIN)

    print("\n[client write methods + debug (set_value/delete, FIX-6)]")
    s,b = req("POST","/api/resource/ToDo", {"description":"ferro-verify-cw"}, user=ADMIN)
    cw = b.get("data",{}).get("name")
    s,b = req("POST","/api/method/frappe.client.set_value", {"doctype":"ToDo","name":cw,"fieldname":"status","value":"Closed"}, user=ADMIN, desk=True)
    check("client.set_value single field", s==200 and b.get("message",{}).get("status")=="Closed", f"{s} {b}")
    # multi-field dict form (use `status`, not the set_only_once `priority` fixture).
    s,b = req("POST","/api/method/frappe.client.set_value", {"doctype":"ToDo","name":cw,"fieldname":{"status":"Open"}}, user=ADMIN, desk=True)
    check("client.set_value multi-field dict", s==200 and b.get("message",{}).get("status")=="Open", f"{s} {b}")
    s,b = req("POST","/api/method/frappe.client.set_value", {"doctype":"ToDo","name":cw,"fieldname":"status","value":"Open"}, user="Guest", desk=True)
    check("Guest client.set_value -> 403 (write gated)", s==403, f"{s} {b}")
    s,b = req("POST","/api/method/frappe.client.delete", {"doctype":"ToDo","name":cw}, user=ADMIN, desk=True)
    check("client.delete -> 200", s==200, f"{s} {b}")
    s,b = req("GET", f"/api/resource/ToDo/{cw}", user=ADMIN)
    check("client.delete actually removed the doc", s==404, f"{s} {b}")
    s,b = req("GET", '/api/resource/DocType?fields=["name"]&limit_page_length=2&debug=1', user=ADMIN)
    check("debug=1 -> _debug_messages present (FIX-6)", s==200 and "_debug_messages" in b, f"keys={list(b)}")

    print("\n[error shapes]")
    s,b = req("GET","/api/resource/NoSuchDoctype/x", user=ADMIN)
    check("unknown doctype -> 404", s==404, f"{s} {b}")
    # _server_messages must be a JSON array of JSON-encoded objects (frappe-js-sdk shape)
    sm = b.get("_server_messages")
    ok_sm = False
    try:
        arr = json.loads(sm); inner = json.loads(arr[0]); ok_sm = isinstance(inner, dict) and "message" in inner
    except Exception: ok_sm = False
    check("_server_messages is JSON array of JSON message objects", ok_sm, f"sm={sm!r}")
    check("exception key omitted in production (no --dev)", "exception" not in b, f"keys={list(b)}")

    print("\n[v2 REST envelope + doctype (B-REST-1/2) + get_user_info (FIX-5)]")
    s,b = req("GET", "/api/v2/document/User", token="bad:bad")
    check("v2 401 uses errors[] envelope, not exc_type (B-REST-1)", s==401 and isinstance(b.get("errors"),list) and "exc_type" not in b, f"{s} {b}")
    s,b = req("GET", "/api/resource/User", token="bad:bad")
    check("v1 401 still uses exc_type envelope", s==401 and b.get("exc_type")=="AuthenticationError", f"{s} {b}")
    s,b = req("GET", "/api/v2/doctype/ToDo/meta", user=ADMIN)
    check("v2 /doctype/<dt>/meta -> {data:{fields:[...]}} (B-REST-2)", s==200 and isinstance(b.get("data",{}).get("fields"),list), f"{s} keys={list(b)}")
    s,b = req("GET", "/api/v2/doctype/DocType/count", user=ADMIN)
    check("v2 /doctype/<dt>/count -> {data:N} (B-REST-2)", s==200 and isinstance(b.get("data"),int) and b["data"]>200, f"{s} {b}")
    s,b = req("GET", "/api/v2/doctype/ToDo/meta", user="Guest")
    check("v2 /doctype/<dt>/meta as Guest -> 403 errors[] (only_for All)", s==403 and isinstance(b.get("errors"),list), f"{s} {b}")
    s,b = req("GET", "/api/method/frappe.realtime.get_user_info?user=Administrator", user=ADMIN, desk=True)
    check("frappe.realtime.get_user_info -> {message:{}} (FIX-5)", s==200 and b.get("message")=={}, f"{s} {b}")

    print("\n[duplicate]")
    s,b = req("POST","/api/resource/ToDo", {"description":"ferro-verify-dup","name":"FERRO-DUP-1"}, user=ADMIN)
    check("create with explicit name", s==200, f"{s} {b}")
    s,b = req("POST","/api/resource/ToDo", {"description":"ferro-verify-dup2","name":"FERRO-DUP-1"}, user=ADMIN)
    check("duplicate name -> 409 DuplicateEntryError", s==409 and b.get("exc_type")=="DuplicateEntryError", f"{s} {b}")
    req("DELETE","/api/resource/ToDo/FERRO-DUP-1", user=ADMIN)

    print("\n[v2 bulk operations + copy (FIX-4, B-REST-3)]")
    n1 = req("POST","/api/resource/ToDo", {"description":"ferro-verify-bulk1"}, user=ADMIN)[1].get("data",{}).get("name")
    n2 = req("POST","/api/resource/ToDo", {"description":"ferro-verify-bulk2"}, user=ADMIN)[1].get("data",{}).get("name")
    n3 = req("POST","/api/resource/ToDo", {"description":"ferro-verify-bulk3"}, user=ADMIN)[1].get("data",{}).get("name")
    # copy: returns the doc without identity fields, marked local
    s,b = req("GET", f"/api/v2/document/ToDo/{n1}/copy", user=ADMIN)
    d = b.get("data",{})
    check("v2 copy strips name + marks __islocal (B-REST-3)", s==200 and "name" not in d and d.get("description")=="ferro-verify-bulk1", f"{s} {d}")
    # v2 document bulk_delete by names
    s,b = req("POST","/api/v2/document/ToDo/bulk_delete", {"names":[n1,n2]}, user=ADMIN)
    dd = b.get("data",{})
    check("v2 document bulk_delete summary (FIX-4)", s==200 and dd.get("success_count")==2 and dd.get("failure_count")==0, f"{s} {b}")
    # v2 method bulk_delete cross-doctype by docs
    s,b = req("POST","/api/v2/method/bulk_delete", {"docs":[{"doctype":"ToDo","name":n3}]}, user=ADMIN)
    check("v2 method bulk_delete cross-doctype (FIX-4)", s==200 and b.get("data",{}).get("success_count")==1, f"{s} {b}")
    # invalid 'docs' -> 417 errors[]
    s,b = req("POST","/api/v2/method/bulk_delete", {"docs":"notalist"}, user=ADMIN)
    check("bulk_delete invalid 'docs' -> 417 errors[] (FIX-4)", s==417 and isinstance(b.get("errors"),list) and "must be a list" in json.dumps(b), f"{s} {b}")

    print("\n[form-encoded body + data field]")
    s,b = req("POST","/api/resource/ToDo", "description=ferro-verify-form&status=Open", user=ADMIN)
    check("form-urlencoded POST body parsed", s==200 and b.get("data",{}).get("description")=="ferro-verify-form", f"{s} {b}")
    if s==200: req("DELETE", f"/api/resource/ToDo/{b['data']['name']}", user=ADMIN)
    s,b = req("POST","/api/resource/ToDo", 'data={"description":"ferro-verify-data"}', user=ADMIN)
    check("FrappeClient 'data' field unwrapped", s==200 and b.get("data",{}).get("description")=="ferro-verify-data", f"{s} {b}")
    if s==200: req("DELETE", f"/api/resource/ToDo/{b['data']['name']}", user=ADMIN)

    print("\n[expand / expand_links (FIX-3)]")
    s,b = req("POST","/api/resource/ToDo", {"description":"ferro-verify-expand","allocated_to":"Administrator"}, user=ADMIN)
    en = b.get("data",{}).get("name")
    s,b = req("GET", f"/api/resource/ToDo/{en}?expand_links=1", user=ADMIN)
    al = b.get("data",{}).get("allocated_to")
    check("expand_links replaces a Link value with the linked doc (FIX-3)", s==200 and isinstance(al,dict) and al.get("name")=="Administrator", f"{s} allocated_to={al!r}")
    s,b = req("GET", f"/api/resource/ToDo/{en}", user=ADMIN)
    check("without expand the Link value stays a string", b.get("data",{}).get("allocated_to")=="Administrator", "")
    s,b = req("GET", f'/api/resource/ToDo?fields=["name","allocated_to"]&filters=[["name","=","{en}"]]&expand=["allocated_to"]', user=ADMIN)
    rows = b.get("data",[])
    check("list expand=[field] expands each row (FIX-3)", s==200 and rows and isinstance(rows[0].get("allocated_to"),dict), f"{s} {rows[:1]}")
    if en: req("DELETE", f"/api/resource/ToDo/{en}", user=ADMIN)

    print("\n[single doctype]")
    s,b = req("GET","/api/resource/Domain Settings/Domain Settings", user=ADMIN)
    check("single read ok + has docstatus/idx", s==200 and "docstatus" in b.get("data",{}) and "idx" in b.get("data",{}), f"{s} {b}")

    print("\n[db-api: client.get_value / get_single_value (B-DB-1/2)]")
    # multi-field get_value returns all requested fields as a dict
    s,b = req("GET", '/api/method/frappe.client.get_value?doctype=User&fieldname=["name","email"]&filters={"name":"Administrator"}', user=ADMIN, desk=True)
    m = b.get("message",{})
    check("get_value multi-field returns all fields", s==200 and m.get("name")=="Administrator" and "email" in m, f"{s} {m}")
    # get_value on a Single reads tabSingles (not a tab<Single> table)
    s,b = req("GET", '/api/method/frappe.client.get_value?doctype=System Settings&fieldname=["app_name"]', user=ADMIN, desk=True)
    check("get_value on a Single reads tabSingles (B-DB-1)", s==200 and b.get("message",{}).get("app_name")=="Frappe", f"{s} {b}")
    # get_list on a Single does not 500
    s,b = req("GET", '/api/method/frappe.client.get_list?doctype=System Settings&fields=["app_name"]', user=ADMIN, desk=True)
    check("get_list on a Single -> one-row list, no 500 (B-DB-1)", s==200 and isinstance(b.get("message"),list) and len(b["message"])==1, f"{s} {b}")
    # get_single_value casts + returns None (not 0) for an unset field
    s,b = req("GET", '/api/method/frappe.client.get_single_value?doctype=System Settings&field=app_name', user=ADMIN, desk=True)
    check("get_single_value returns the set value", s==200 and b.get("message")=="Frappe", f"{s} {b}")
    s,b = req("GET", '/api/method/frappe.client.get_single_value?doctype=System Settings&field=__nope__', user=ADMIN, desk=True)
    check("get_single_value unset field -> null not 0 (B-DB-2)", s==200 and b.get("message") is None, f"{s} {b}")

    print("\n[meta: Custom Field + Property Setter (B-MET-1/2)]")
    s,b = req("POST","/api/resource/ToDo", {"description":"ferro-verify-meta"}, user=ADMIN)
    tname = b.get("data",{}).get("name")
    s,b = req("GET", f"/api/resource/ToDo/{tname}", user=ADMIN)
    check("admin sees the merged Custom Field column (B-MET-1)", "ferro_cf" in b.get("data",{}), f"keys={list(b.get('data',{}))[:14]}")
    s,b = req("GET", f"/api/resource/ToDo/{tname}", token=TTOK)
    check("permlevel-0 reader does NOT see permlevel-1 Custom Field (B-MET-1 permlevel merged)", s==200 and "ferro_cf" not in b.get("data",{}), f"{s} keys={list(b.get('data',{}))[:14]}")
    # Without the Property Setter, birth_date is a permlevel-0 column visible to the permlevel-0 reader.
    s,b = req("GET","/api/resource/User/Administrator", token=TTOK)
    check("Property Setter permlevel override masks birth_date for permlevel-0 reader (B-MET-2)", "birth_date" not in b.get("data",{}), f"keys={list(b.get('data',{}))}")
    s,b = req("GET","/api/resource/User/Administrator", user=ADMIN)
    check("admin still sees the Property-Setter field birth_date", "birth_date" in b.get("data",{}), "")
    if tname: req("DELETE", f"/api/resource/ToDo/{tname}", user=ADMIN)

    print("\n[parent-key corruption (FIX-8)]")
    s,b = req("GET", "/api/resource/User/Administrator", user=ADMIN)
    d = b.get("data",{})
    bad = [k for k in d if "parent" in k or k.startswith('"')]
    check("non-child doc (User) has NO parent*/quoted phantom keys", s==200 and not bad, f"bad keys={bad}")
    # A child row still carries its real parent/parentfield/parenttype columns.
    s,b = req("GET", "/api/resource/Has Role/frt-role-1", user=ADMIN)
    d = b.get("data",{})
    check("child doc (Has Role) keeps real parent linkage", s==200 and d.get("parent")=="ferro_test@example.com" and d.get("parentfield")=="roles" and d.get("parenttype")=="User", f"{s} parent={d.get('parent')!r} pf={d.get('parentfield')!r} pt={d.get('parenttype')!r}")

    print("\n[read-only / maintenance mode (B-REST-4)]")
    cfg_path = SITE + "/site_config.json"
    _cfg = json.load(open(cfg_path))
    try:
        json.dump({**_cfg, "maintenance_mode": 1}, open(cfg_path, "w"))
        s,b = req("POST","/api/resource/ToDo", {"description":"ferro-verify-ro"}, user=ADMIN)
        check("write in maintenance mode -> 503 InReadOnlyMode (v1)", s==503 and b.get("exc_type")=="InReadOnlyMode", f"{s} {b}")
        s,b = req("POST","/api/v2/document/ToDo", {"description":"ferro-verify-ro"}, user=ADMIN)
        check("write in maintenance mode -> 503 errors[] (v2)", s==503 and isinstance(b.get("errors"),list), f"{s} {b}")
        s,b = req("GET","/api/resource/DocType?limit_page_length=1", user=ADMIN)
        check("read still works in maintenance mode -> 200", s==200, f"{s} {b}")
    finally:
        json.dump(_cfg, open(cfg_path, "w"))

    print("\n[virtual doctype]")
    s,b = req("GET","/api/resource/Recorder", user=ADMIN)
    check("virtual doctype list -> [] not 500", s==200 and b.get("data")==[], f"{s} {b}")
    s,b = req("GET","/api/resource/Recorder/x", user=ADMIN)
    check("virtual doctype get -> 404 not 500", s==404, f"{s} {b}")

if __name__ == "__main__":
    print("=== ferro functional verification ===")
    cleanup()  # in case of prior run
    setup()
    try:
        run_tests()
    finally:
        cleanup()
    print(f"\n=== RESULT: {PASS} passed, {FAIL} failed ===")
    sys.exit(1 if FAIL else 0)
