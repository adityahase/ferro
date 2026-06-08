#!/usr/bin/env python3
"""
ferro_bench_shim.py — the bench-command router.

`bench <cmd>` execs `python -m frappe.utils.bench_helper frappe <cmd>`. When ferro is switched on,
`contrib/bench-ferro-switch.sh` installs THIS file in place of frappe's `bench_helper.py` (saving the
original next to it as `bench_helper_ferro_orig.py`). On every invocation it decides, from the SAME
`common_site_config.json` source of truth the web-runtime switch uses:

  * if  web_runtime == "ferro"  AND the subcommand is in FERRO_SUPPORTED  AND the ferro binary exists
        -> re-exec the ferro binary as `ferro bench <args...>` (native, no Python interpreter).
  * otherwise
        -> call the ORIGINAL frappe bench_helper main(), byte-identically. Nothing changes.

This makes existing tooling (`bench --site X migrate`, deploy scripts, CI, supervisor) transparently
route through ferro for the commands it implements, while everything else still runs real Python.
Fully reversible: `bench-ferro-switch.sh off` restores the original bench_helper.py.

Safe-by-default: FERRO_SUPPORTED is a conservative allowlist of commands ferro has VERIFIED native
coverage for. Anything not listed falls through to Python. ferro's own `bench` dispatch ALSO falls
through to Python for anything it declines, so there is a second safety net.
"""

import json
import os
import sys

# Commands ferro implements natively. Keep this conservative — only verified-native commands.
FERRO_SUPPORTED = {
    # config + site meta
    "set-config", "use", "set-maintenance-mode", "list-sites", "show-config", "list-apps", "version",
    # auth + sessions
    "set-admin-password", "set-password", "destroy-all-sessions",
    # scheduler toggles
    "enable-scheduler", "disable-scheduler", "scheduler",
    # cache
    "clear-cache", "clear-website-cache",
    # backup / restore / teardown
    "backup", "restore", "drop-site", "db-console", "sqlite", "bypass-patch",
    # schema + install lifecycle
    "reload-doctype", "reload-doc", "install-app", "migrate", "new-site",
    "remove-from-installed-apps",
    # db ops + users
    "disable-user", "describe-database-table", "add-database-index", "trim-database",
    "ready-for-migration",
}

GLOBAL_OPTS_WITH_VALUE = {"--site", "-s", "--sites-path"}
GLOBAL_FLAGS = {"--force", "--verbose", "-v", "--profile"}


def _sites_path():
    # bench runs from its root; sites/ holds common_site_config.json (mirror bench_helper get_sites).
    return "sites" if os.path.isdir("sites") else "."


def _common_config():
    try:
        with open(os.path.join(_sites_path(), "common_site_config.json")) as f:
            return json.load(f)
    except Exception:
        return {}


def _find_subcommand(args):
    """Given the args after the prog name (starting with the 'frappe' group token), return the
    subcommand name, skipping the group token and the global options."""
    if args and args[0] == "frappe":
        args = args[1:]
    i = 0
    while i < len(args):
        a = args[i]
        if a in GLOBAL_OPTS_WITH_VALUE:
            i += 2
            continue
        if a in GLOBAL_FLAGS:
            i += 1
            continue
        if a.startswith("-"):
            i += 1
            continue
        return a
    return None


def _ferro_args(argv):
    """Strip the leading 'frappe' click-group token; ferro's `bench` arm mirrors the global opts."""
    return argv[1:] if argv and argv[0] == "frappe" else argv


def _run_original():
    """Call the saved original bench_helper.main() — byte-identical Python fallthrough."""
    try:
        from frappe.utils import bench_helper_ferro_orig as orig
    except Exception:
        # not installed as the in-package shim (e.g. run standalone): fall back to the live module.
        from frappe.utils import bench_helper as orig
    orig.main()


def main():
    argv = sys.argv[1:]
    cfg = _common_config()
    runtime = cfg.get("web_runtime", "gunicorn")
    ferro_bin = os.environ.get("FERRO_BIN") or cfg.get("ferro_bin")
    sub = _find_subcommand(argv)

    if (
        runtime == "ferro"
        and sub in FERRO_SUPPORTED
        and ferro_bin
        and os.path.exists(ferro_bin)
    ):
        ferro_argv = [ferro_bin, "bench"] + _ferro_args(argv)
        os.execv(ferro_bin, ferro_argv)
        # execv never returns on success

    _run_original()


if __name__ == "__main__":
    main()
