#!/usr/bin/env python3
"""Measurement TARGET process.

Does the setup work for a given mode, prints "READY <pid>" to stdout (so the
driver knows the heap is warm), then blocks on SIGTERM. While it blocks the
driver snapshots /proc/<pid>/smaps_rollup and /proc/<pid>/smaps.

Modes:
  bare            - interpreter only (no frappe)
  bare_imports    - + the representative stdlib import set
  import_frappe   - import frappe
  init_connect    - frappe.init(site) + frappe.connect()
  warm_meta       - + get_meta on a few doctypes (a "warm worker")
  get_all         - init/connect + a request-like read

For fork/COW testing this same file is driven by fork_cow.py instead.
"""
import os
import sys
import signal


def setup(mode):
    if mode == "bare":
        return
    if mode == "bare_imports":
        import importlib
        for m in ("os,sys,json,re,collections,datetime,sqlite3,hashlib,"
                  "base64,functools,itertools,logging").split(","):
            importlib.import_module(m)
        return
    # frappe modes
    import frappe
    if mode == "import_frappe":
        return
    site = os.environ["MB_SITE"]
    frappe.init(site=site)
    frappe.connect()
    if mode == "init_connect":
        return
    if mode == "warm_meta":
        for dt in ("User", "DocType", "DocField", "Role", "Report",
                   "Custom Field", "Workflow", "Print Format", "File",
                   "Email Queue"):
            try:
                frappe.get_meta(dt)
            except Exception:
                pass
        return
    if mode == "get_all":
        frappe.get_all("DocType", limit=20)
        return


def main():
    mode = sys.argv[1]
    setup(mode)
    # Touch nothing more; report readiness and freeze so the heap is stable
    # while the driver reads /proc.
    sys.stdout.write(f"READY {os.getpid()}\n")
    sys.stdout.flush()
    signal.signal(signal.SIGTERM, lambda *_: os._exit(0))
    signal.pause()


if __name__ == "__main__":
    main()
