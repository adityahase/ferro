#!/usr/bin/env python3
"""Probe the glibc malloc heap after the realistic workload, and test how much
RSS is reclaimable (fragmentation / high-water-mark) vs genuinely live.

Reports:
  - glibc mallinfo2: arena (brk total), in-use, free-in-heap, mmap'd
  - RSS before/after malloc_trim(0)            -> reclaimable glibc fragmentation
  - RSS before/after pymalloc arena reclaim     -> via gc + (no direct trim)
  - mimalloc presence
Run with bench env python, cwd = sites/.
"""
import os
import sys
import gc
import ctypes

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)


def rss_kib():
    with open("/proc/self/statm") as f:
        pages = int(f.read().split()[1])
    return pages * (os.sysconf("SC_PAGE_SIZE") // 1024)


def rollup():
    d = {}
    with open("/proc/self/smaps_rollup") as f:
        for line in f:
            if line.startswith(("Rss:", "Pss:", "Private_Dirty:", "Anonymous:")):
                k, v = line.split(":")
                d[k.strip()] = int(v.split()[0])
    return d


class mallinfo2(ctypes.Structure):
    _fields_ = [(n, ctypes.c_size_t) for n in
                ("arena", "ordblks", "smblks", "hblks", "hblkhd", "usmblks",
                 "fsmblks", "uordblks", "fordblks", "keepcost")]


def main():
    import workload
    workload.run_workload(rounds=int(os.environ.get("MB_ROUNDS", "3")))
    gc.collect(); gc.collect()

    libc = ctypes.CDLL("libc.so.6", use_errno=True)
    libc.mallinfo2.restype = mallinfo2
    mi = libc.mallinfo2()

    def MB(x): return x / 1024 / 1024
    print("=== glibc mallinfo2 (main arena only) ===")
    print(f"  arena    (brk heap total) : {MB(mi.arena):8.2f} MB")
    print(f"  uordblks (in use)         : {MB(mi.uordblks):8.2f} MB")
    print(f"  fordblks (free in heap)   : {MB(mi.fordblks):8.2f} MB   <- fragmentation retained")
    print(f"  hblkhd   (mmap'd blocks)  : {MB(mi.hblkhd):8.2f} MB")
    print(f"  keepcost (top free)       : {MB(mi.keepcost):8.2f} MB")
    print(f"  ordblks  (free chunks #)  : {mi.ordblks}")

    r0 = rollup()
    print(f"\nRSS before trim: {r0['Rss']/1024:.2f} MB  (Anon {r0['Anonymous']/1024:.2f}, "
          f"PrivDirty {r0['Private_Dirty']/1024:.2f})")

    # malloc_trim returns free memory from the top of the heap to the OS
    libc.malloc_trim.restype = ctypes.c_int
    ret = libc.malloc_trim(0)
    r1 = rollup()
    print(f"malloc_trim(0) returned {ret}; RSS after: {r1['Rss']/1024:.2f} MB  "
          f"(reclaimed {(r0['Rss']-r1['Rss'])/1024:.2f} MB)")

    mi2 = libc.mallinfo2()
    print(f"  after-trim arena {MB(mi2.arena):.2f} MB  fordblks {MB(mi2.fordblks):.2f} MB")

    # Is mimalloc actually mapped?
    mimalloc = False
    try:
        with open("/proc/self/maps") as f:
            mimalloc = "mimalloc" in f.read()
    except OSError:
        pass
    print(f"\nmimalloc mapped: {mimalloc}")
    print(f"PYTHONMALLOC={os.environ.get('PYTHONMALLOC','(unset)')}")


if __name__ == "__main__":
    main()
