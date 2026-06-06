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

def req(method, url, body=None, token=None, user=None):
    args = [FERRO, "request", SITE, method, url]
    if body is not None:
        args.append(body if isinstance(body, str) else json.dumps(body))
    if token:
        args += ["--token", token]
    if user:
        args += ["--user", user]
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

    print("\n[duplicate]")
    s,b = req("POST","/api/resource/ToDo", {"description":"ferro-verify-dup","name":"FERRO-DUP-1"}, user=ADMIN)
    check("create with explicit name", s==200, f"{s} {b}")
    s,b = req("POST","/api/resource/ToDo", {"description":"ferro-verify-dup2","name":"FERRO-DUP-1"}, user=ADMIN)
    check("duplicate name -> 409 DuplicateEntryError", s==409 and b.get("exc_type")=="DuplicateEntryError", f"{s} {b}")
    req("DELETE","/api/resource/ToDo/FERRO-DUP-1", user=ADMIN)

    print("\n[form-encoded body + data field]")
    s,b = req("POST","/api/resource/ToDo", "description=ferro-verify-form&status=Open", user=ADMIN)
    check("form-urlencoded POST body parsed", s==200 and b.get("data",{}).get("description")=="ferro-verify-form", f"{s} {b}")
    if s==200: req("DELETE", f"/api/resource/ToDo/{b['data']['name']}", user=ADMIN)
    s,b = req("POST","/api/resource/ToDo", 'data={"description":"ferro-verify-data"}', user=ADMIN)
    check("FrappeClient 'data' field unwrapped", s==200 and b.get("data",{}).get("description")=="ferro-verify-data", f"{s} {b}")
    if s==200: req("DELETE", f"/api/resource/ToDo/{b['data']['name']}", user=ADMIN)

    print("\n[single doctype]")
    s,b = req("GET","/api/resource/Domain Settings/Domain Settings", user=ADMIN)
    check("single read ok + has docstatus/idx", s==200 and "docstatus" in b.get("data",{}) and "idx" in b.get("data",{}), f"{s} {b}")

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
