#!/usr/bin/env python3
"""In-process memory introspection — the authoritative snapshot.

dump(outdir, tag) captures, for the CURRENT process, at the current heap state:

  1. /proc/self/smaps_rollup  -> parsed RSS/PSS/USS/anon/file  (kernel truth)
  2. /proc/self/smaps         -> full per-VMA map (saved raw for VMA analysis)
  3. sys._debugmallocstats()  -> obmalloc arena/pool/block accounting (1 MiB arenas)
  4. full live-heap walk      -> bytes + count bucketed by type (incl. str/int/bytes
                                 which gc.get_objects() omits), via referents BFS
  5. per-file code-object bytes (static code cost attribution)
  6. gc stats + module count

Authoritative RSS/PSS/USS are read FIRST (cheap), before the heavy census
allocates anything, so the headline numbers are not perturbed by measurement.

Pure stdlib only (no third-party imports) so it can run in the bare baseline too.
"""
import gc
import io
import os
import sys
import json
import contextlib

ROLLUP_KEYS = ("Rss", "Pss", "Pss_Anon", "Pss_File", "Shared_Clean", "Shared_Dirty",
               "Private_Clean", "Private_Dirty", "Referenced", "Anonymous",
               "AnonHugePages", "Swap", "Locked")


def read_smaps_rollup():
    out = {}
    try:
        with open("/proc/self/smaps_rollup") as f:
            for line in f:
                k = line.split(":", 1)[0]
                if k in ROLLUP_KEYS:
                    out[k] = int(line.split()[1])  # KiB
    except OSError:
        pass
    out["USS"] = out.get("Private_Clean", 0) + out.get("Private_Dirty", 0)
    out["RSS"] = out.get("Rss", 0)
    out["PSS"] = out.get("Pss", 0)
    return out


def save_smaps(path):
    try:
        with open("/proc/self/smaps") as f, open(path, "w") as g:
            g.write(f.read())
    except OSError as e:
        with open(path, "w") as g:
            g.write(f"error: {e}\n")


def dump_mallocstats(path):
    # _debugmallocstats writes to stderr; capture by temporarily duping fd 2.
    sys.stderr.flush()
    saved = os.dup(2)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o644)
    os.dup2(fd, 2)
    os.close(fd)
    try:
        sys._debugmallocstats()
        sys.stderr.flush()
    finally:
        os.dup2(saved, 2)
        os.close(saved)


def heap_census(max_objs=8_000_000):
    """Walk the full reachable object graph (incl. atomic str/int/bytes) and
    bucket shallow getsizeof by type. dedup by id. Returns (by_type, totals)."""
    sizeof = sys.getsizeof
    # roots: everything gc tracks, plus module dicts, plus this frame's globals
    roots = gc.get_objects()
    roots.append(sys.modules)
    seen = set()
    seen_add = seen.add
    # exclude our own bookkeeping containers from the census
    exclude = {id(roots), id(seen)}
    by_count = {}
    by_size = {}
    total = 0
    n = 0
    stack = roots
    get_referents = gc.get_referents
    while stack:
        obj = stack.pop()
        oid = id(obj)
        if oid in seen or oid in exclude:
            continue
        seen_add(oid)
        n += 1
        if n > max_objs:
            break
        try:
            sz = sizeof(obj)
        except Exception:
            sz = 0
        tname = type(obj).__name__
        by_count[tname] = by_count.get(tname, 0) + 1
        by_size[tname] = by_size.get(tname, 0) + sz
        total += sz
        try:
            refs = get_referents(obj)
        except Exception:
            continue
        for r in refs:
            if id(r) not in seen:
                stack.append(r)
    totals = {"n_objects": n, "total_bytes": total, "truncated": n > max_objs}
    return by_count, by_size, totals


def code_object_bytes_by_file(max_objs=8_000_000):
    """Sum getsizeof of all code objects, attributed to co_filename -> bytes.
    Captures the *bytecode/code-object* static cost per source file."""
    import types
    sizeof = sys.getsizeof
    seen = set()
    by_file = {}
    by_file_count = {}
    total = 0
    stack = gc.get_objects()
    get_referents = gc.get_referents
    n = 0
    while stack:
        obj = stack.pop()
        oid = id(obj)
        if oid in seen:
            continue
        seen.add(oid)
        n += 1
        if n > max_objs:
            break
        if isinstance(obj, types.CodeType):
            fn = obj.co_filename
            sz = sizeof(obj)
            # include consts that are themselves code/str/bytes attributed here
            by_file[fn] = by_file.get(fn, 0) + sz
            by_file_count[fn] = by_file_count.get(fn, 0) + 1
            total += sz
        try:
            for r in get_referents(obj):
                if id(r) not in seen:
                    stack.append(r)
        except Exception:
            pass
    return by_file, by_file_count, total


def module_top_packages():
    """Count modules per top-level package."""
    pkgs = {}
    for name in list(sys.modules):
        top = name.split(".", 1)[0]
        pkgs[top] = pkgs.get(top, 0) + 1
    return pkgs


def dump(outdir, tag):
    os.makedirs(outdir, exist_ok=True)
    p = lambda n: os.path.join(outdir, f"{tag}.{n}")

    # settle the heap
    gc.collect(); gc.collect()

    # 1+2: kernel truth FIRST (before census perturbs RSS)
    rollup = read_smaps_rollup()
    save_smaps(p("smaps.txt"))

    # 3: arena accounting
    try:
        dump_mallocstats(p("mallocstats.txt"))
    except Exception as e:
        with open(p("mallocstats.txt"), "w") as f:
            f.write(f"error: {e}\n")

    # gc stats
    gcinfo = {
        "get_count": gc.get_count(),
        "get_stats": gc.get_stats(),
        "n_tracked": len(gc.get_objects()),
        "n_modules": len(sys.modules),
        "thresholds": gc.get_threshold(),
    }

    # 4: heap census (allocates — done after RSS snapshot)
    by_count, by_size, totals = heap_census()
    top_by_size = sorted(by_size.items(), key=lambda kv: kv[1], reverse=True)[:80]
    census = {
        "totals": totals,
        "top_by_size": [{"type": t, "bytes": b, "count": by_count.get(t, 0)}
                        for t, b in top_by_size],
    }

    # 5: code-object bytes per file
    cob_file, cob_count, cob_total = code_object_bytes_by_file()
    top_files = sorted(cob_file.items(), key=lambda kv: kv[1], reverse=True)[:120]
    codecensus = {
        "total_code_bytes": cob_total,
        "n_files": len(cob_file),
        "top_files": [{"file": f, "bytes": b, "n_code_objs": cob_count.get(f, 0)}
                      for f, b in top_files],
    }

    # module packages
    pkgs = module_top_packages()
    top_pkgs = sorted(pkgs.items(), key=lambda kv: kv[1], reverse=True)[:40]

    out = {
        "tag": tag,
        "pid": os.getpid(),
        "rollup_kib": rollup,
        "gc": gcinfo,
        "census": census,
        "code_by_file": codecensus,
        "top_packages": top_pkgs,
    }
    with open(p("summary.json"), "w") as f:
        json.dump(out, f, indent=2, default=str)

    # human-readable headline to stdout
    print(json.dumps({
        "tag": tag, "pid": os.getpid(),
        "RSS_MB": round(rollup.get("RSS", 0) / 1024, 2),
        "PSS_MB": round(rollup.get("PSS", 0) / 1024, 2),
        "USS_MB": round(rollup.get("USS", 0) / 1024, 2),
        "Anon_MB": round(rollup.get("Anonymous", 0) / 1024, 2),
        "n_modules": gcinfo["n_modules"],
        "n_tracked": gcinfo["n_tracked"],
        "census_total_MB": round(totals["total_bytes"] / 1024 / 1024, 2),
        "census_n_objects": totals["n_objects"],
        "code_bytes_MB": round(cob_total / 1024 / 1024, 2),
    }))
    return out
