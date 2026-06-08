#!/usr/bin/env python3
"""Measure the Python-side floor: import the frappe shim + all 5 apps' controllers, report RSS."""
import sys, os, gc, time
_HERE = os.path.dirname(os.path.abspath(__file__))
_ROOT = os.path.dirname(os.path.dirname(_HERE))
sys.path.insert(0, os.path.join(_ROOT, "framework", "shim"))   # deduplicated shim
os.environ.setdefault(
    "FERRO_REPOS", os.path.join(_ROOT, "docs", "investigations", "apps-64mb", "repos"))

def rss_mb():
    for l in open("/proc/self/status"):
        if l.startswith("VmRSS:"):
            return int(l.split()[1]) / 1024.0
    return -1

def hwm_mb():
    for l in open("/proc/self/status"):
        if l.startswith("VmHWM:"):
            return int(l.split()[1]) / 1024.0
    return -1

mode = sys.argv[1] if len(sys.argv) > 1 else "all"
apps = sys.argv[2].split(",") if len(sys.argv) > 2 else None

print(f"bare              RSS={rss_mb():.1f}")
import frappe
print(f"+frappe shim      RSS={rss_mb():.1f}  modules={len(sys.modules)}")
import ferro_boot
t0 = time.time()
stats = ferro_boot.load(apps=apps, mode=mode, catch_all=True, verbose=("-v" in sys.argv))
dt = time.time() - t0
gc.collect()
print(f"+controllers      RSS={rss_mb():.1f}  HWM={hwm_mb():.1f}  modules={len(sys.modules)}  ({dt:.1f}s)")
print(f"stats: {stats}")
s = ferro_boot.summary()
print(f"doctypes={s['doctypes']} controllers={s['controllers']} import_errors={s['import_errors']} "
      f"stubbed_modules={s['stubbed']} doctypes_with_hooks={s['merged_doctypes_with_hooks']}")
reg = ferro_boot.needs_python_registry()
print(f"needs-python: doctypes={len(reg['by_doctype'])} wildcard_events={reg['wildcard']}")
print(f"stubbed sample: {s['stubbed_top'][:25]}")
