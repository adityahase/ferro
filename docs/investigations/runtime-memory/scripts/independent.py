#!/usr/bin/env python3
"""Measure N INDEPENDENT warm Frappe workers alive at once (the current model:
each gunicorn/RQ worker imports frappe itself — no shared parent heap).

Spawns N copies of target.py warm_meta, waits for all READY, then reads
/proc/<pid>/smaps_rollup for each while all are alive. With siblings present,
the kernel now marks shared file-backed pages (libpython, the C-extension .so
code) as Shared_Clean across them — but each worker's object-graph heap stays
Private_Dirty (shared with no one). PSS captures the real per-box footprint.

argv: N
"""
import json
import os
import subprocess
import sys
import time

ROLLUP = ("Rss", "Pss", "Shared_Clean", "Shared_Dirty",
          "Private_Clean", "Private_Dirty")


def rollup(pid):
    out = dict.fromkeys(ROLLUP, 0)
    with open(f"/proc/{pid}/smaps_rollup") as f:
        for line in f:
            for k in ROLLUP:
                if line.startswith(k + ":"):
                    out[k] = int(line.split()[1])
    out["USS"] = out["Private_Clean"] + out["Private_Dirty"]
    return out


def main():
    n = int(sys.argv[1])
    py = sys.executable
    here = os.path.dirname(os.path.abspath(__file__))
    target = os.path.join(here, "target.py")

    procs, pids = [], []
    for _ in range(n):
        p = subprocess.Popen([py, target, "warm_meta"],
                             stdout=subprocess.PIPE, text=True)
        procs.append(p)
    # collect READY pids
    for p in procs:
        deadline = time.time() + 120
        while time.time() < deadline:
            line = p.stdout.readline()
            if line.startswith("READY"):
                pids.append(int(line.split()[1]))
                break
            if not line and p.poll() is not None:
                break

    time.sleep(1.0)
    rolls = [rollup(pid) for pid in pids]
    result = {
        "model": "independent", "n": n, "workers": rolls,
        "agg_rss": sum(r["Rss"] for r in rolls),
        "agg_pss": sum(r["Pss"] for r in rolls),
        "agg_uss": sum(r["USS"] for r in rolls),
    }
    print(json.dumps(result))

    for p in procs:
        p.terminate()
    for p in procs:
        try:
            p.wait(timeout=5)
        except subprocess.TimeoutExpired:
            p.kill()


if __name__ == "__main__":
    main()
