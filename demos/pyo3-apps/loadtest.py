#!/usr/bin/env python3
"""Load-test a running ferrod and sample its memory (RSS/PSS/USS) under request churn."""
import sys, time, json, threading, urllib.request, urllib.parse, os

PORT = sys.argv[1] if len(sys.argv) > 1 else "8090"
PID = sys.argv[2] if len(sys.argv) > 2 else None
ROUNDS = int(sys.argv[3]) if len(sys.argv) > 3 else 6
BASE = f"http://127.0.0.1:{PORT}"

READ_DOCTYPES = ["CRM Deal", "CRM Lead", "HD Ticket", "GP Project", "Employee",
                 "Sales Invoice", "Item", "Customer", "Contact", "ToDo"]


def req(method, path, body=None):
    url = BASE + path
    data = json.dumps(body).encode() if body is not None else None
    r = urllib.request.Request(url, data=data, method=method,
                               headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(r, timeout=30) as resp:
            return resp.status, resp.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()
    except Exception as e:
        return 0, str(e).encode()


def smaps(pid):
    rss = pss = pc = pd = 0.0
    try:
        for l in open(f"/proc/{pid}/smaps_rollup"):
            p = l.split()
            if l.startswith("Rss:"): rss = int(p[1]) / 1024
            elif l.startswith("Pss:"): pss = int(p[1]) / 1024
            elif l.startswith("Private_Clean:"): pc = int(p[1]) / 1024
            elif l.startswith("Private_Dirty:"): pd = int(p[1]) / 1024
    except FileNotFoundError:
        return None
    return rss, pss, pc + pd


peak = [0.0, 0.0, 0.0]
stop = threading.Event()


def sampler():
    while not stop.is_set():
        m = smaps(PID)
        if m:
            for i in range(3):
                peak[i] = max(peak[i], m[i])
        time.sleep(0.05)


def worker(n, results):
    ok = err = 0
    enc = urllib.parse.quote
    for r in range(ROUNDS):
        for dt in READ_DOCTYPES:
            s, _ = req("GET", f"/api/resource/{enc(dt)}?limit_page_length=5")
            ok += s == 200; err += s != 200
            s, _ = req("GET", f"/api/resource/{enc(dt)}?fields=%5B%22name%22%5D&limit_page_length=20")
            ok += s == 200; err += s != 200
        # a Python-driven write (CRM Deal has a controller validate)
        s, b = req("POST", "/api/resource/CRM Deal",
                   {"organization": f"Load-{n}-{r}", "status": "Qualification", "deal_owner": "Administrator"})
        ok += s == 200; err += s != 200
        # a pure-CRUD write (ToDo: frappe-core doctype, no app controller)
        s, b = req("POST", "/api/resource/ToDo", {"description": f"todo-{n}-{r}", "status": "Open"})
        ok += s in (200, 417, 409); err += s not in (200, 417, 409)
    results[n] = (ok, err)


if PID:
    threading.Thread(target=sampler, daemon=True).start()

NTHREADS = 8
results = {}
threads = [threading.Thread(target=worker, args=(i, results)) for i in range(NTHREADS)]
t0 = time.time()
for t in threads: t.start()
for t in threads: t.join()
dt = time.time() - t0
time.sleep(0.3)
stop.set()
time.sleep(0.1)

ok = sum(r[0] for r in results.values()); err = sum(r[1] for r in results.values())
print(f"requests: ok={ok} err={err}  in {dt:.1f}s  ({(ok+err)/dt:.0f} req/s)")
if PID:
    print(f"PEAK under load:  RSS={peak[0]:.1f} MB   PSS={peak[1]:.1f} MB   USS={peak[2]:.1f} MB")
    final = smaps(PID)
    if final:
        print(f"FINAL (post-load): RSS={final[0]:.1f} MB   PSS={final[1]:.1f} MB   USS={final[2]:.1f} MB")
    print(f"under 64 MB: {'YES' if peak[0] < 64 else 'NO'}")
