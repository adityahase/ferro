#!/usr/bin/env bash
# Clean baseline: min-of-N peak RSS (KiB) per (interpreter, scenario).
# min is used because peak RSS only ever inflates under noise; the floor is the
# truest measure of the interpreter's intrinsic footprint.
set -u
N=3
GTIME=/usr/bin/time
PV="$HOME/.pyenv/versions"

# interpreter table: label|binary|importlib?(1/0)
INTERPS=(
  "CPython-3.12.13|$PV/3.12.13/bin/python|1"
  "CPython-3.13.13|$PV/3.13.13/bin/python|1"
  "CPython-3.14.4-system|/usr/bin/python3|1"
  "PyPy-7.3.22-py3.11|$PV/pypy3.11-7.3.22/bin/pypy3|1"
  "MicroPython-1.28.0|$PV/micropython-1.28.0/bin/micropython|0"
)
# Add RustPython if it built.
if [ -x "$HOME/rustpython-bin/bin/rustpython" ]; then
  INTERPS+=("RustPython-git|$HOME/rustpython-bin/bin/rustpython|1")
fi

IMPORTS="os,sys,json,re,collections,datetime,sqlite3,hashlib,base64,functools,itertools,logging"

measure() { # $1=binary $2=code  -> min peak rss kib over N runs (or NA)
  local bin="$1" code="$2" best="" v
  for _ in $(seq "$N"); do
    v=$("$GTIME" -f '%M' "$bin" -c "$code" 2>/tmp/_mberr >/dev/null; tail -1 /tmp/_mberr 2>/dev/null)
    # keep only if numeric
    case "$v" in (''|*[!0-9]*) continue;; esac
    if [ -z "$best" ] || [ "$v" -lt "$best" ]; then best="$v"; fi
  done
  [ -z "$best" ] && echo "NA" || echo "$best"
}

echo "label,scenario,peak_rss_kib_min_of_${N}"
for row in "${INTERPS[@]}"; do
  IFS='|' read -r label bin hasimportlib <<<"$row"
  [ -x "$bin" ] || { echo "$label,MISSING_BINARY,NA"; continue; }
  echo "$label,bare,$(measure "$bin" 'pass')"
  if [ "$hasimportlib" = "1" ]; then
    echo "$label,stdlib_imports,$(measure "$bin" "import importlib
for m in '${IMPORTS}'.split(','):
    importlib.import_module(m)")"
  else
    echo "$label,stdlib_imports,NA_no_importlib"
  fi
  echo "$label,list_1M_ints,$(measure "$bin" 'x=[i for i in range(1000000)]')"
done
