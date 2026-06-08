#!/usr/bin/env python3
"""Build the heap up to a named stage, then dump an authoritative snapshot.

Usage:  stage_run.py <stage> <outdir>
Stages (each is a superset of the previous):
  bare      - interpreter + introspect module only
  import    - + import frappe (and frappe.app)
  connect   - + frappe.init(site) + frappe.connect()
  warmmeta  - + set_user(Administrator) + meta/controller warmup (no requests)
  workload  - + full realistic web-worker workload (ACTUAL USAGE)  <-- primary

cwd must be the bench sites/ dir; run with the bench env python.
"""
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import introspect  # noqa: E402


def build(stage):
    if stage == "bare":
        return
    import frappe          # noqa: F401
    import frappe.app      # noqa: F401
    if stage == "import":
        return
    frappe.init(site=os.environ.get("MB_SITE", "mysite.sqlite"), sites_path=".")
    frappe.connect()
    if stage == "connect":
        return
    frappe.set_user("Administrator")
    import workload
    if stage == "warmmeta":
        workload._warm_meta(frappe)
        return
    if stage == "workload":
        workload.run_workload(rounds=int(os.environ.get("MB_ROUNDS", "3")))
        return
    raise SystemExit(f"unknown stage {stage!r}")


def main():
    stage = sys.argv[1]
    outdir = sys.argv[2]
    build(stage)
    introspect.dump(outdir, stage)


if __name__ == "__main__":
    main()
