#!/usr/bin/env python3
"""Attribute a process's memory to mappings: who do we share with?

Groups /proc/<pid>/smaps entries by backing file (or anon region kind) and sums
Rss / Pss / Shared_Clean / Shared_Dirty / Private_Dirty per group. File-backed
read-only pages (Shared_Clean) are the pages shared with every other process
that maps the same file (libpython, libc, the C-extension .so's). Anonymous +
Private_Dirty is the per-process heap (the object graph) that is shared with
no one in the independent-worker model.

Usage: analyze_smaps.py <smaps_file> [top_n]
"""
import sys
from collections import defaultdict

FIELDS = ("Rss", "Pss", "Shared_Clean", "Shared_Dirty",
          "Private_Clean", "Private_Dirty")


def classify(path):
    if not path:
        return "[anon/heap]"
    if path.startswith("[") and path.endswith("]"):
        return path
    base = path.rsplit("/", 1)[-1]
    return base


def main():
    fn = sys.argv[1]
    top_n = int(sys.argv[2]) if len(sys.argv) > 2 else 25
    groups = defaultdict(lambda: dict.fromkeys(FIELDS, 0))
    cur = None
    with open(fn) as f:
        for line in f:
            parts = line.split()
            # header line of a mapping: addr perms off dev inode [path]
            if len(parts) >= 5 and "-" in parts[0] and parts[1][:1] in "r-":
                path = parts[5] if len(parts) >= 6 else ""
                cur = classify(path)
                continue
            if cur is None:
                continue
            for fld in FIELDS:
                if line.startswith(fld + ":"):
                    groups[cur][fld] += int(line.split()[1])

    totals = dict.fromkeys(FIELDS, 0)
    for g in groups.values():
        for fld in FIELDS:
            totals[fld] += g[fld]

    rows = sorted(groups.items(), key=lambda kv: kv[1]["Pss"], reverse=True)
    print(f"{'mapping':32} {'Rss':>8} {'Pss':>8} {'ShClean':>8} "
          f"{'PrivDirty':>9}  (KiB)")
    print("-" * 72)
    for name, g in rows[:top_n]:
        print(f"{name[:32]:32} {g['Rss']:8d} {g['Pss']:8d} "
              f"{g['Shared_Clean']:8d} {g['Private_Dirty']:9d}")
    print("-" * 72)
    print(f"{'TOTAL':32} {totals['Rss']:8d} {totals['Pss']:8d} "
          f"{totals['Shared_Clean']:8d} {totals['Private_Dirty']:9d}")
    shared = totals["Shared_Clean"] + totals["Shared_Dirty"]
    uss = totals["Private_Clean"] + totals["Private_Dirty"]
    print(f"\nSharable (file-backed Shared_Clean+Dirty): {shared} KiB "
          f"({shared/1024:.1f} MB)")
    print(f"Private/unshared (USS): {uss} KiB ({uss/1024:.1f} MB)")


if __name__ == "__main__":
    main()
