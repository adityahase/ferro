#!/usr/bin/env python3
"""Per-module memory attribution for a running Frappe worker (CPython 3.14).

The aggregate footprint is already known (import frappe ~+49 MB, warm worker ~115 MB).
This script attributes that footprint to individual modules / top-level packages,
from four complementary angles. Each angle has a documented blind spot, so they are
reported together.

Modes (argv[1]):
  incremental  - ONE process. Import Frappe's real startup chain step by step and
                 sample resident memory (VmRSS) after each step. Shows the *marginal*
                 RSS each step adds, in context. RSS includes native/C-ext memory.
  tracemalloc  - Import frappe, then warm a worker (init+connect+get_meta+get_all),
                 and attribute *Python-heap* bytes to the module that allocated them,
                 rolled up to top-level package. Two snapshots: after `import frappe`,
                 and after warm-up (to localize the +49 MB meta jump). Python-heap
                 bytes EXCLUDE C-extension/native memory (orjson/cryptography/lxml/...).
  census       - Import frappe + warm worker. Census of sys.modules grouped by
                 top-level package: module count, summed code-object bytes, and a
                 deep getsizeof of each module's namespace. Structural breadth view.

Output: CSV to stdout. Memory in KiB for RSS, bytes for tracemalloc/sizeof.
Site is taken from argv[2] (default mysite.sqlite). Run from the bench `sites/` dir.
"""
import sys
import gc

SITE = sys.argv[2] if len(sys.argv) > 2 else "mysite.sqlite"


def vmrss_kib():
    """Current resident set size in KiB (instantaneous)."""
    with open("/proc/self/status") as f:
        for line in f:
            if line.startswith("VmRSS:"):
                return int(line.split()[1])
    return -1


def vmhwm_kib():
    """Peak resident set size so far in KiB (high-water mark)."""
    with open("/proc/self/status") as f:
        for line in f:
            if line.startswith("VmHWM:"):
                return int(line.split()[1])
    return -1


def warm_worker():
    """Bring a worker to a realistic warm state: init+connect+meta+read."""
    import frappe
    frappe.init(site=SITE)
    frappe.connect()
    metas = [
        "User", "DocType", "Role", "DocField", "DocPerm",
        "System Settings", "Report", "Page", "Module Def", "Custom Field",
        "Workspace", "Web Page", "Email Queue", "File", "Comment",
        "Version", "Activity Log", "Notification Log", "Print Format", "Property Setter",
    ]
    for dt in metas:
        try:
            frappe.get_meta(dt)
        except Exception:
            pass
    try:
        frappe.get_all("User", limit=20)
    except Exception:
        pass
    return frappe


# ----------------------------------------------------------------------------- incremental
def mode_incremental():
    """Import Frappe's real chain step by step; sample VmRSS after each step."""
    g = {}

    def step_import(stmt):
        return lambda: exec(stmt, g)

    steps = [
        ("00_interpreter_start", lambda: None),
        ("01_stdlib_core", step_import(
            "import os,sys,re,json,functools,importlib,inspect,threading,warnings,collections")),
        ("02_orjson", step_import("import orjson")),
        ("03_werkzeug_headers", step_import("from werkzeug.datastructures import Headers")),
        ("04_import_frappe", step_import("import frappe")),
        ("05_frappe_init", step_import(f"frappe.init(site={SITE!r})")),
        ("06_frappe_connect", step_import("frappe.connect()")),
        ("07_get_meta_User", step_import("frappe.get_meta('User')")),
        ("08_get_meta_DocType", step_import("frappe.get_meta('DocType')")),
        ("09_get_meta_18_more", step_import(
            "import frappe as _f\n"
            "for _d in ['Role','DocField','DocPerm','System Settings','Report','Page',"
            "'Module Def','Custom Field','Workspace','Web Page','Email Queue','File','Comment',"
            "'Version','Activity Log','Notification Log','Property Setter','Print Format',"
            "'Email Template','Web Form']:\n"
            "    try:\n        _f.get_meta(_d)\n    except Exception:\n        pass")),
        ("10_get_all_User", step_import("frappe.get_all('User', limit=20)")),
    ]

    print("step,vmrss_kib,vmhwm_kib,delta_kib,modules_loaded")
    prev = None
    for label, fn in steps:
        try:
            fn()
        except Exception as e:
            sys.stderr.write(f"[incremental] step {label} failed: {e!r}\n")
        gc.collect()  # settle transient allocations so the delta reflects retained memory
        rss = vmrss_kib()
        hwm = vmhwm_kib()
        nmods = len(sys.modules)
        delta = "" if prev is None else rss - prev
        print(f"{label},{rss},{hwm},{delta},{nmods}")
        prev = rss


# ----------------------------------------------------------------------------- tracemalloc
def _top_pkg(filename):
    """Map a source filename to a top-level package/group label."""
    import os
    fn = filename or "<unknown>"
    norm = fn.replace("\\", "/")
    parts = norm.split("/")
    # site-packages/<pkg>/...  -> site:<pkg>
    for marker in ("site-packages", "dist-packages"):
        if marker in parts:
            i = parts.index(marker)
            if i + 1 < len(parts):
                name = parts[i + 1]
                return "site:" + (name[:-3] if name.endswith(".py") else name)
    # Frappe APP code lives at .../apps/frappe/frappe/<sub>/... . Anchor on "apps"
    # (NOT on the literal segment "frappe" -- the host's home dir is /home/frappe,
    #  which would otherwise capture every frappe path as the home dir).
    if "apps" in parts:
        i = parts.index("apps")
        # parts[i+1]=frappe(app), parts[i+2]=frappe(pkg), parts[i+3]=<sub>
        if i + 3 < len(parts):
            sub = parts[i + 3]
            if sub.endswith(".py"):
                return "frappe.__root__"
            return "frappe." + sub
        return "frappe.__root__"
    # stdlib: python3.14/<mod>.py or python3.14/<pkg>/...
    for p in parts:
        if p.startswith("python3."):
            i = parts.index(p)
            if i + 1 < len(parts):
                name = parts[i + 1]
                return "stdlib:" + (name[:-3] if name.endswith(".py") else name)
    if fn.startswith("<"):
        return "interp:" + fn  # <frozen ...>, <string>, etc.
    return "other:" + os.path.basename(fn)


def _snapshot_by_pkg(snapshot):
    """Roll a tracemalloc snapshot up to top-level package -> total bytes, blocks."""
    agg = {}
    for stat in snapshot.statistics("filename"):
        frame = stat.traceback[0]
        key = _top_pkg(frame.filename)
        size, count = agg.get(key, (0, 0))
        agg[key] = (size + stat.size, count + stat.count)
    return agg


def mode_tracemalloc():
    import tracemalloc
    tracemalloc.start(1)  # group by filename; 1 frame suffices

    import frappe  # noqa: F401
    gc.collect()
    snap_import = tracemalloc.take_snapshot()
    rss_after_import = vmrss_kib()
    traced_import, _ = tracemalloc.get_traced_memory()

    warm_worker()
    gc.collect()
    snap_warm = tracemalloc.take_snapshot()
    rss_after_warm = vmrss_kib()
    traced_warm, _ = tracemalloc.get_traced_memory()

    agg_import = _snapshot_by_pkg(snap_import)
    agg_warm = _snapshot_by_pkg(snap_warm)

    print("== tracemalloc_per_package ==")
    print("package,bytes_after_import,bytes_after_warm,delta_warm_minus_import,blocks_warm")
    keys = sorted(set(agg_import) | set(agg_warm),
                  key=lambda k: -agg_warm.get(k, (0, 0))[0])
    for k in keys:
        bi = agg_import.get(k, (0, 0))[0]
        bw, cw = agg_warm.get(k, (0, 0))
        print(f"{k},{bi},{bw},{bw - bi},{cw}")

    print("== tracemalloc_totals ==")
    print("metric,value")
    print(f"traced_python_bytes_after_import,{traced_import}")
    print(f"traced_python_bytes_after_warm,{traced_warm}")
    print(f"vmrss_kib_after_import,{rss_after_import}")
    print(f"vmrss_kib_after_warm,{rss_after_warm}")
    print(f"vmrss_bytes_after_import,{rss_after_import * 1024}")
    print(f"vmrss_bytes_after_warm,{rss_after_warm * 1024}")
    # NOTE: the two rows below are ARTIFACTS, NOT a native-memory estimate. vmrss_* here is
    # measured UNDER tracemalloc, whose per-allocation bookkeeping inflates RSS ~2.5x (see
    # README-tracemalloc-note.txt). For the real native footprint, diff the clean (non-tracemalloc)
    # RSS from incremental.csv/isolation.csv against traced_python_bytes_*. Kept only for provenance.
    print(f"ARTIFACT_IGNORE_native_gap_bytes_after_import,{rss_after_import * 1024 - traced_import}")
    print(f"ARTIFACT_IGNORE_native_gap_bytes_after_warm,{rss_after_warm * 1024 - traced_warm}")

    print("== tracemalloc_top_files_warm ==")
    print("rank,bytes,blocks,filename")
    for i, stat in enumerate(snap_warm.statistics("filename")[:60], 1):
        frame = stat.traceback[0]
        print(f"{i},{stat.size},{stat.count},{frame.filename}")


# ----------------------------------------------------------------------------- census
def _code_size(co, seen):
    """getsizeof of a code object + its bytecode + consts (incl. nested code), once."""
    oid = id(co)
    if oid in seen:
        return 0
    seen.add(oid)
    total = 0
    try:
        total += sys.getsizeof(co)
        total += sys.getsizeof(getattr(co, "co_code", b""))
        consts = getattr(co, "co_consts", ())
        total += sys.getsizeof(consts)
        for c in consts:
            # nested code objects (methods, comprehensions, closures)
            if type(c).__name__ == "code":
                total += _code_size(c, seen)
            else:
                total += sys.getsizeof(c, 0)
    except Exception:
        pass
    return total


def _module_code_bytes(mod, seen):
    """Sum code-object bytes for all functions/methods defined in a module."""
    total = 0
    try:
        items = list(getattr(mod, "__dict__", {}).values())  # snapshot (threads mutate)
    except Exception:
        return 0
    for v in items:
        try:
            co = getattr(v, "__code__", None)
            if co is not None:
                total += _code_size(co, seen)
                continue
            # classes: walk their methods' code objects
            if isinstance(v, type):
                try:
                    cvals = list(vars(v).values())
                except Exception:
                    cvals = []
                for m in cvals:
                    mco = getattr(m, "__code__", None)
                    if mco is not None:
                        total += _code_size(mco, seen)
        except Exception:
            pass
    return total


def mode_census():
    warm_worker()
    gc.collect()

    rss = vmrss_kib()
    snapshot = list(sys.modules.items())  # snapshot to avoid concurrent-mutation errors
    nmods = len(snapshot)

    groups = {}  # top-level package -> [(name, module)]
    for name, mod in snapshot:
        if mod is None:
            continue
        top = name.split(".")[0] if name else "<empty>"
        groups.setdefault(top, []).append((name, mod))

    print("== census_summary ==")
    print("metric,value")
    print(f"total_modules,{nmods}")
    print(f"top_level_packages,{len(groups)}")
    print(f"vmrss_kib_warm,{rss}")

    print("== census_per_package ==")
    print("top_package,module_count,code_bytes")
    seen = set()  # global: count each code object once, attributed to first package touched
    rows = []
    for top, mods in groups.items():
        code_bytes = 0
        for name, mod in mods:
            code_bytes += _module_code_bytes(mod, seen)
        rows.append((top, len(mods), code_bytes))
    for top, cnt, cb in sorted(rows, key=lambda r: -r[2]):
        print(f"{top},{cnt},{cb}")


if __name__ == "__main__":
    mode = sys.argv[1] if len(sys.argv) > 1 else "incremental"
    {"incremental": mode_incremental,
     "tracemalloc": mode_tracemalloc,
     "census": mode_census}[mode]()
