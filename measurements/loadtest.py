#!/usr/bin/env python3
"""Load generator for ferro. Fills the meta cache (all doctypes) and hammers
read + CRUD paths concurrently, so we can measure peak resident memory."""
import json, sys, time, threading, urllib.request, urllib.error
from concurrent.futures import ThreadPoolExecutor

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 18080
TOKEN = sys.argv[2] if len(sys.argv) > 2 else None
ROUNDS = int(sys.argv[3]) if len(sys.argv) > 3 else 3
SRV_PID = int(sys.argv[4]) if len(sys.argv) > 4 else None
BASE = f"http://127.0.0.1:{PORT}"

# ---- memory sampler (peak RSS/PSS/USS of the server process) ----
_peak = {"rss": 0, "pss": 0, "uss": 0}
_stop = threading.Event()
def _sample():
    if not SRV_PID:
        return
    while not _stop.is_set():
        try:
            with open(f"/proc/{SRV_PID}/smaps_rollup") as fh:
                rss = pss = pd = pc = 0
                for ln in fh:
                    if ln.startswith("Rss:"): rss = int(ln.split()[1])
                    elif ln.startswith("Pss:"): pss = int(ln.split()[1])
                    elif ln.startswith("Private_Dirty:"): pd = int(ln.split()[1])
                    elif ln.startswith("Private_Clean:"): pc = int(ln.split()[1])
                uss = pd + pc
                _peak["rss"] = max(_peak["rss"], rss)
                _peak["pss"] = max(_peak["pss"], pss)
                _peak["uss"] = max(_peak["uss"], uss)
        except Exception:
            pass
        time.sleep(0.02)
_mon = threading.Thread(target=_sample, daemon=True)
_mon.start()

def req(method, path, body=None):
    url = BASE + path
    data = json.dumps(body).encode() if body is not None else None
    r = urllib.request.Request(url, data=data, method=method)
    if TOKEN:
        r.add_header("Authorization", "token " + TOKEN)
    if data:
        r.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(r, timeout=30) as resp:
            return resp.status, json.loads(resp.read() or b"{}")
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"{}")
    except Exception as e:
        return -1, {"err": str(e)}

# 1. Get all doctype names
st, res = req("GET", '/api/resource/DocType?fields=["name"]&limit_page_length=100000')
names = [d["name"] for d in res.get("data", [])]
print(f"doctypes discovered: {len(names)}")

def load_meta(dt):
    # get_list with limit 5 forces meta load + a real query
    from urllib.parse import quote
    return req("GET", f"/api/resource/{quote(dt)}?limit_page_length=5")

errors = []
def hammer_round(rnd):
    with ThreadPoolExecutor(max_workers=16) as ex:
        futs = [ex.submit(load_meta, dt) for dt in names]
        for dt, f in zip(names, futs):
            st, body = f.result()
            if st not in (200, 403, 404, 500):
                errors.append((dt, st, body))

t0 = time.time()
total = 0
for rnd in range(ROUNDS):
    hammer_round(rnd)
    total += len(names)
print(f"meta+list requests: {total} over {ROUNDS} rounds in {time.time()-t0:.1f}s")
print(f"non-OK statuses (sample): {errors[:5]}  count={len(errors)}")

# 2. CRUD cycle on ToDo (a simple writable doctype) x N
def crud_cycle(i):
    st, body = req("POST", "/api/resource/ToDo", {"description": f"load test {i}", "status": "Open"})
    if st != 200:
        return ("create", st, body)
    name = body.get("data", {}).get("name")
    req("PUT", f"/api/resource/ToDo/{name}", {"status": "Closed"})
    req("GET", f"/api/resource/ToDo/{name}")
    req("DELETE", f"/api/resource/ToDo/{name}")
    return None

with ThreadPoolExecutor(max_workers=8) as ex:
    crud_res = [f for f in (ex.submit(crud_cycle, i) for i in range(200))]
    crud_errs = [f.result() for f in crud_res]
    crud_errs = [e for e in crud_errs if e]
print(f"CRUD cycles: 200, errors: {len(crud_errs)} sample={crud_errs[:3]}")
_stop.set(); _mon.join(timeout=1)
if SRV_PID:
    print(f"PEAK rss={_peak['rss']}kB pss={_peak['pss']}kB uss={_peak['uss']}kB")
print("DONE")
