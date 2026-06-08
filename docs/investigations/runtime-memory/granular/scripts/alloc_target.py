#!/usr/bin/env python3
"""Run the identical workload, then report RSS and post-trim RSS.
Allocator is selected by the caller via LD_PRELOAD / MALLOC_* / PYTHONMALLOC env.
Prints one line: LABEL RSS_MB=.. RSS_trim_MB=.. ANON_MB=.."""
import os
import sys
import gc
import ctypes

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)


def rollup():
    d = {}
    try:
        with open("/proc/self/smaps_rollup") as f:
            for line in f:
                if line.startswith(("Rss:", "Anonymous:")):
                    k, v = line.split(":")
                    d[k.strip()] = int(v.split()[0])
    except OSError:
        pass
    return d


def main():
    label = sys.argv[1] if len(sys.argv) > 1 else "?"
    import workload
    workload.run_workload(rounds=int(os.environ.get("MB_ROUNDS", "3")))
    gc.collect(); gc.collect()
    r0 = rollup()
    rss0 = r0.get("Rss", 0)
    anon = r0.get("Anonymous", 0)

    # glibc malloc_trim (no-op / harmless under other allocators)
    rss1 = rss0
    try:
        libc = ctypes.CDLL("libc.so.6")
        libc.malloc_trim.restype = ctypes.c_int
        libc.malloc_trim(0)
        rss1 = rollup().get("Rss", 0)
    except Exception:
        pass

    print(f"{label}\tRSS_MB={rss0/1024:.2f}\tRSS_trim_MB={rss1/1024:.2f}\tANON_MB={anon/1024:.2f}")


if __name__ == "__main__":
    main()
