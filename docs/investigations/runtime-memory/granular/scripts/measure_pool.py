#!/usr/bin/env python3
"""Sum memory across a gunicorn master+worker process tree, reporting the metrics
that matter for forked pools: total RSS (over-counts shared), total PSS (the real
RAM footprint — shared pages split across sharers), and total private-dirty.

Usage: measure_pool.py <master_pid> <label>
"""
import os
import sys


def rollup(pid):
    d = {}
    try:
        with open(f"/proc/{pid}/smaps_rollup") as f:
            for line in f:
                for k in ("Rss", "Pss", "Private_Dirty", "Private_Clean",
                          "Shared_Clean", "Shared_Dirty"):
                    if line.startswith(k + ":"):
                        d[k] = int(line.split()[1])
    except OSError:
        return None
    return d


def children(pid):
    try:
        with open(f"/proc/{pid}/task/{pid}/children") as f:
            return [int(x) for x in f.read().split()]
    except OSError:
        return []


def main():
    master = int(sys.argv[1])
    label = sys.argv[2] if len(sys.argv) > 2 else "?"
    pids = [master] + children(master)
    tot = {"Rss": 0, "Pss": 0, "Private_Dirty": 0}
    n = 0
    per = []
    for pid in pids:
        r = rollup(pid)
        if not r:
            continue
        n += 1
        for k in tot:
            tot[k] += r.get(k, 0)
        per.append((pid, r.get("Rss", 0) / 1024, r.get("Pss", 0) / 1024,
                    r.get("Private_Dirty", 0) / 1024))
    print(f"{label}: {n} procs (1 master + {n-1} workers)")
    print(f"  TOTAL  RSS {tot['Rss']/1024:8.1f} MB   PSS {tot['Pss']/1024:8.1f} MB   "
          f"PrivDirty {tot['Private_Dirty']/1024:8.1f} MB")
    if n > 1:
        print(f"  per-worker avg PSS {tot['Pss']/1024/n:.1f} MB")
    for pid, rss, pss, pd in per:
        print(f"    pid {pid:7d}  RSS {rss:7.1f}  PSS {pss:7.1f}  PrivDirty {pd:7.1f}")


if __name__ == "__main__":
    main()
