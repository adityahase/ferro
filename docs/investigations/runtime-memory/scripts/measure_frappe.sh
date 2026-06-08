#!/usr/bin/env bash
# Measure the memory footprint of the Frappe framework under a given interpreter/bench.
#
# Usage: measure_frappe.sh <label> <bench-env-python> <bench-dir> <site>
# Emits CSV rows: label,scenario,peak_rss_kib,exit_rc
#
# Scenarios (each in a fresh process; peak RSS via /usr/bin/time -f '%M'):
#   import_frappe   - cost of importing the framework package
#   init_connect    - frappe.init(site) + frappe.connect()  (what a request worker holds)
#   load_meta       - + load DocType metadata for a core doctype (warm worker)
set -u
LABEL="${1:?label}"; PY="${2:?env python}"; BENCH="${3:?bench dir}"; SITE="${4:?site}"
GTIME=/usr/bin/time
SITES_DIR="$BENCH/sites"

peak() { "$GTIME" -f '%M' "$PY" -c "$1" 2>&1 >/dev/null | tail -1; }

# import frappe
code_import='import frappe'
echo "${LABEL},import_frappe,$(cd "$SITES_DIR" && peak "$code_import"),$?"

# init + connect to the SQLite-backed site
code_init="import frappe
frappe.init(site='${SITE}')
frappe.connect()
"
echo "${LABEL},init_connect,$(cd "$SITES_DIR" && peak "$code_init"),$?"

# init + connect + load metadata for a core DocType (warm request worker)
code_meta="import frappe
frappe.init(site='${SITE}')
frappe.connect()
frappe.get_meta('User')
frappe.get_meta('DocType')
"
echo "${LABEL},load_meta,$(cd "$SITES_DIR" && peak "$code_meta"),$?"
