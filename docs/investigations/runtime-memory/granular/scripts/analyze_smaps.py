#!/usr/bin/env python3
"""Account every VMA in a /proc/<pid>/smaps dump.

Buckets each mapping by kind (file-backed .so / .py-bytecode-cache / db file /
anon heap / stack / special) and reports resident (Rss) per bucket and per
named file, plus EVERY individual mapping whose Rss >= threshold (default 256
KiB) so nothing >= a quarter-MB goes unattributed.

Usage: analyze_smaps.py <smaps.txt> [rss_threshold_kib=256]
"""
import sys
import re
import os
from collections import defaultdict

HEADER = re.compile(
    r'^([0-9a-f]+)-([0-9a-f]+)\s+(\S+)\s+([0-9a-f]+)\s+(\S+)\s+(\d+)\s*(.*)$')


def classify(path, perms):
    if not path:
        return "anon"
    if path.startswith("[stack"):
        return "stack"
    if path in ("[heap]",):
        return "heap_brk"
    if path.startswith("["):
        return "special:" + path
    base = os.path.basename(path)
    if base.endswith(".so") or ".so." in base:
        return "lib_so"
    if base.endswith((".db", ".sqlite", ".sqlite3")):
        return "dbfile"
    if path.endswith((".py", ".pyc")) or "__pycache__" in path:
        return "pyfile"
    if "/T/" in path or path.startswith("/tmp") or "memfd" in path:
        return "tmp"
    return "file_other"


def main():
    smaps = sys.argv[1]
    thr = int(sys.argv[2]) if len(sys.argv) > 2 else 256  # KiB

    maps = []
    cur = None
    with open(smaps) as f:
        for line in f:
            m = HEADER.match(line)
            if m:
                if cur:
                    maps.append(cur)
                start = int(m.group(1), 16)
                end = int(m.group(2), 16)
                perms = m.group(3)
                path = m.group(7).strip()
                cur = {"start": start, "end": end, "perms": perms,
                       "path": path, "size_kib": (end - start) // 1024,
                       "kind": classify(path, perms)}
            else:
                if cur is None:
                    continue
                if ":" in line:
                    k, _, v = line.partition(":")
                    v = v.strip()
                    if v.endswith("kB"):
                        try:
                            cur[k.strip()] = int(v.split()[0])
                        except ValueError:
                            pass
    if cur:
        maps.append(cur)

    by_kind_rss = defaultdict(int)
    by_kind_size = defaultdict(int)
    by_kind_count = defaultdict(int)
    by_file_rss = defaultdict(int)
    total_rss = total_size = 0
    for mp in maps:
        rss = mp.get("Rss", 0)
        by_kind_rss[mp["kind"]] += rss
        by_kind_size[mp["kind"]] += mp["size_kib"]
        by_kind_count[mp["kind"]] += 1
        total_rss += rss
        total_size += mp["size_kib"]
        if mp["kind"] in ("lib_so", "dbfile", "file_other", "pyfile"):
            by_file_rss[os.path.basename(mp["path"])] += rss

    print(f"=== TOTAL: Rss {total_rss/1024:.1f} MB  Vsize {total_size/1024:.1f} MB  "
          f"({len(maps)} mappings) ===\n")

    print("=== Rss by KIND ===")
    for k in sorted(by_kind_rss, key=lambda x: by_kind_rss[x], reverse=True):
        print(f"  {k:18s} Rss {by_kind_rss[k]/1024:8.2f} MB  "
              f"Vsize {by_kind_size[k]/1024:8.2f} MB  ({by_kind_count[k]} maps)")

    print("\n=== Rss by FILE (libs/db/pyfiles), >= 0.10 MB ===")
    for fn in sorted(by_file_rss, key=lambda x: by_file_rss[x], reverse=True):
        if by_file_rss[fn] >= 100:
            print(f"  {by_file_rss[fn]/1024:8.2f} MB  {fn}")

    print(f"\n=== EVERY mapping with Rss >= {thr} KiB ===")
    big = [mp for mp in maps if mp.get("Rss", 0) >= thr]
    big.sort(key=lambda m: m.get("Rss", 0), reverse=True)
    accounted = 0
    for mp in big:
        rss = mp.get("Rss", 0)
        accounted += rss
        anon = mp.get("Anonymous", 0)
        name = mp["path"] or f"(anon {mp['perms']})"
        print(f"  Rss {rss/1024:8.3f} MB  anon {anon/1024:7.2f}  "
              f"size {mp['size_kib']/1024:7.2f}  {mp['kind']:10s} {os.path.basename(name) if mp['path'] else name}")
    print(f"\n  >= {thr}KiB mappings account for {accounted/1024:.1f} MB of "
          f"{total_rss/1024:.1f} MB ({100*accounted/total_rss:.1f}%); "
          f"{(total_rss-accounted)/1024:.1f} MB in smaller maps")


if __name__ == "__main__":
    main()
