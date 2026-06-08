#!/usr/bin/env python3
"""Measure copy-on-write sharing across forked Frappe workers (the gunicorn
--preload model) vs the independent-import model.

Parent imports frappe + warms the meta cache ONCE, then forks N children.
Forked children share the parent's heap pages copy-on-write. We then read
/proc/<pid>/smaps_rollup for the parent and every child and report:
  - per-process USS (truly private) and PSS (proportional, shares divided)
  - aggregate RSS (naive sum) vs aggregate PSS (real footprint on the box)

argv: N [do_work=0|1] [freeze=0|1]
  do_work=1  -> each child runs a request-like read after fork, which makes
                CPython write refcounts into shared object headers, eroding COW.
  freeze=1   -> call gc.freeze() before fork so the cyclic GC won't traverse
                (and thus dirty) the pre-fork object graph.
Prints one JSON object.
"""
import gc
import json
import os
import signal
import sys
import time

ROLLUP = ("Rss", "Pss", "Shared_Clean", "Shared_Dirty",
          "Private_Clean", "Private_Dirty")


def rollup(pid):
    out = dict.fromkeys(ROLLUP, 0)
    try:
        with open(f"/proc/{pid}/smaps_rollup") as f:
            for line in f:
                for k in ROLLUP:
                    if line.startswith(k + ":"):
                        out[k] = int(line.split()[1])
    except FileNotFoundError:
        return None
    out["USS"] = out["Private_Clean"] + out["Private_Dirty"]
    return out


def main():
    n = int(sys.argv[1])
    do_work = len(sys.argv) > 2 and sys.argv[2] == "1"
    freeze = len(sys.argv) > 3 and sys.argv[3] == "1"

    import frappe
    site = os.environ["MB_SITE"]
    frappe.init(site=site)
    frappe.connect()
    for dt in ("User", "DocType", "DocField", "Role", "Report",
               "Custom Field", "Workflow", "Print Format", "File",
               "Email Queue"):
        try:
            frappe.get_meta(dt)
        except Exception:
            pass

    if freeze:
        gc.collect()
        gc.freeze()  # move surviving objects out of GC's view; GC won't dirty them

    children = []
    for _ in range(n):
        pid = os.fork()
        if pid == 0:
            # child
            if do_work:
                try:
                    frappe.get_all("DocType", limit=50)
                    for dt in ("User", "DocType", "Role"):
                        frappe.get_meta(dt)
                except Exception:
                    pass
            signal.signal(signal.SIGTERM, lambda *_: os._exit(0))
            signal.pause()
            os._exit(0)
        children.append(pid)

    time.sleep(1.0)  # let children settle / COW faults land

    parent_pid = os.getpid()
    result = {
        "n": n, "do_work": do_work, "freeze": freeze,
        "parent": rollup(parent_pid),
        "children": [rollup(c) for c in children],
    }
    # aggregates over the whole family (parent + children)
    fam = [result["parent"]] + [c for c in result["children"] if c]
    result["agg_rss"] = sum(p["Rss"] for p in fam)
    result["agg_pss"] = sum(p["Pss"] for p in fam)
    result["agg_uss"] = sum(p["USS"] for p in fam)
    print(json.dumps(result))

    for c in children:
        try:
            os.kill(c, signal.SIGTERM)
        except ProcessLookupError:
            pass


if __name__ == "__main__":
    main()
