#!/usr/bin/env bash
# Apples-to-apples interpreter memory baseline.
# For a given interpreter binary, measures peak RSS (KiB) of a child process for
# three scenarios, using GNU time's %M (max resident set size).
#
# Usage: measure_baseline.sh <label> <interpreter-binary> [stdlib-import-list]
# Emits a CSV row:  label,scenario,peak_rss_kib,exit_rc
set -u
LABEL="${1:?label}"
PY="${2:?interpreter path}"
# A representative-but-portable import set. CPython/PyPy have all of these;
# MicroPython/RustPython will fail some — that failure is itself recorded.
IMPORTS="${3:-os,sys,json,re,collections,datetime,sqlite3,hashlib,base64,functools,itertools,logging}"

# GNU time (not the bash builtin). Fall back gracefully.
GTIME="$(command -v /usr/bin/time || true)"
peak() { # $1 = code string
  if [ -n "$GTIME" ]; then
    # %M is max RSS in KiB on Linux
    out=$("$GTIME" -f '%M' "$PY" -c "$1" 2>&1 >/dev/null)
    rc=$?
    # last line of stderr is the %M number (or an error)
    echo "$out" | tail -1
    return $rc
  else
    echo "NA"; return 0
  fi
}

# Scenario 1: bare interpreter (does nothing)
v=$(peak 'pass'); rc=$?
echo "${LABEL},bare,${v},${rc}"

# Scenario 2: after importing the stdlib set
code2="import importlib
mods='${IMPORTS}'.split(',')
for m in mods:
    importlib.import_module(m)"
v=$(peak "$code2"); rc=$?
echo "${LABEL},stdlib_imports,${v},${rc}"

# Scenario 3: hold a fixed data structure (1,000,000 ints in a list) — per-object overhead
code3='x=[i for i in range(1000000)]
import sys'
v=$(peak "$code3"); rc=$?
echo "${LABEL},list_1M_ints,${v},${rc}"
