#!/usr/bin/env python3
"""tracemalloc attribution: start tracing BEFORE importing frappe, run the full
workload, then report Python-level allocations by source file:line and grouped
by top-level package. tracemalloc sees allocations routed through PyMem/obmalloc
(i.e. Python objects + most interpreter allocs); it does NOT see raw malloc done
inside C extensions that bypass PyMem (those show up under memray --native).

Usage: tracemalloc_run.py <outdir>. cwd = sites/, bench env python.
"""
import os
import sys
import json
import tracemalloc

NFRAME = 30
tracemalloc.start(NFRAME)

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)


def main():
    outdir = sys.argv[1]
    os.makedirs(outdir, exist_ok=True)
    import workload
    workload.run_workload(rounds=int(os.environ.get("MB_ROUNDS", "3")))

    import gc
    gc.collect()
    snap = tracemalloc.take_snapshot()
    cur, peak = tracemalloc.get_traced_memory()

    # by file:line
    stats_line = snap.statistics("lineno")
    top_line = [{"loc": str(s.traceback), "size": s.size, "count": s.count}
                for s in stats_line[:120]]

    # by filename
    stats_file = snap.statistics("filename")
    top_file = [{"file": str(s.traceback), "size": s.size, "count": s.count}
                for s in stats_file[:80]]

    # group by top-level package (derived from path segment after site-packages/apps)
    pkg = {}
    pkgc = {}
    for s in stats_file:
        f = str(s.traceback)
        name = "?"
        for marker in ("site-packages/", "/apps/", "python3.14/"):
            if marker in f:
                rest = f.split(marker, 1)[1]
                name = rest.split("/", 1)[0].split(":")[0]
                if marker == "python3.14/":
                    name = "stdlib:" + name
                break
        else:
            name = f
        pkg[name] = pkg.get(name, 0) + s.size
        pkgc[name] = pkgc.get(name, 0) + s.count
    top_pkg = sorted(pkg.items(), key=lambda kv: kv[1], reverse=True)[:60]

    out = {
        "traced_current_MB": round(cur / 1024 / 1024, 2),
        "traced_peak_MB": round(peak / 1024 / 1024, 2),
        "n_frames": NFRAME,
        "top_by_line": top_line,
        "top_by_file": top_file,
        "by_package": [{"pkg": k, "MB": round(v / 1024 / 1024, 3), "count": pkgc[k]}
                       for k, v in top_pkg],
    }
    with open(os.path.join(outdir, "tracemalloc.json"), "w") as f:
        json.dump(out, f, indent=2)

    print(json.dumps({"traced_current_MB": out["traced_current_MB"],
                      "traced_peak_MB": out["traced_peak_MB"]}))
    print("\n=== tracemalloc: top Python allocation sites by total live bytes ===")
    for e in top_line[:35]:
        loc = e["loc"].replace(HERE, ".")
        print(f"  {e['size']/1024/1024:7.3f} MB  n={e['count']:<7} {loc[-110:]}")
    print("\n=== by package ===")
    for k, v in top_pkg[:30]:
        print(f"  {v/1024/1024:7.3f} MB  {k}")


if __name__ == "__main__":
    main()
