#!/usr/bin/env bash
#
# bench-ferro-switch.sh — flip a Frappe bench's web runtime between CPython (gunicorn/bench serve)
# and ferro, with ONE reversible change. Nothing else in the bench is touched: sites/, assets/,
# the Node socketio process, workers, scheduler, and nginx all stay exactly as-is.
#
# The source of truth is `sites/common_site_config.json` -> "web_runtime": "ferro" | "gunicorn".
# For dev (`bench start`) we rewrite the Procfile's `web:` line; the original is saved alongside
# so `off` restores it byte-for-byte. For prod, this script prints the supervisor/systemd patch.
#
# Usage:
#   bench-ferro-switch.sh on        # web runtime -> ferro
#   bench-ferro-switch.sh off       # web runtime -> gunicorn/bench serve (default)
#   bench-ferro-switch.sh status    # show the current runtime
#   bench-ferro-switch.sh prod      # print the supervisor/systemd patch for production
#
# Env:
#   FERRO_BIN   path to the ferro binary (default: the built release binary in this repo, or
#               `ferro` on PATH). For a faithful drop-in across the whole bench, build `ferrod`
#               (ferro + embedded CPython) and point FERRO_BIN at it.
#   BENCH_DIR   the bench root (default: autodetected by walking up to a dir containing sites/).
#   FERRO_PORT  bind port (default: webserver_port from common_site_config.json, else 8000).
#   FERRO_THREADS  worker threads (default: gunicorn_workers from common_site_config.json, else 5).

set -euo pipefail

# --- locate the ferro binary -------------------------------------------------
default_bin="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/target/release/ferro"
FERRO_BIN="${FERRO_BIN:-$default_bin}"
if [[ ! -x "$FERRO_BIN" ]]; then
  if command -v ferro >/dev/null 2>&1; then FERRO_BIN="$(command -v ferro)"; fi
fi

# --- locate the bench root (a dir containing sites/common_site_config.json) ---
find_bench() {
  local d="${BENCH_DIR:-$PWD}"
  while [[ "$d" != "/" ]]; do
    if [[ -f "$d/sites/common_site_config.json" ]]; then echo "$d"; return 0; fi
    d="$(dirname "$d")"
  done
  return 1
}
BENCH_DIR="$(find_bench)" || { echo "error: not inside a bench (no sites/common_site_config.json found)"; exit 1; }
CSC="$BENCH_DIR/sites/common_site_config.json"
PROCFILE="$BENCH_DIR/Procfile"

py() { python3 "$@"; }

get_cfg() { # get_cfg KEY DEFAULT
  py - "$CSC" "$1" "$2" <<'PY'
import json,sys
cfg=json.load(open(sys.argv[1]))
print(cfg.get(sys.argv[2], sys.argv[3]))
PY
}

set_runtime() { # set_runtime VALUE
  py - "$CSC" "$1" <<'PY'
import json,sys
p=sys.argv[1]; cfg=json.load(open(p))
cfg["web_runtime"]=sys.argv[2]
json.dump(cfg, open(p,"w"), indent=1)
PY
}

PORT="${FERRO_PORT:-$(get_cfg webserver_port 8000)}"
THREADS="${FERRO_THREADS:-$(get_cfg gunicorn_workers 5)}"
# The all-in-one ferro web command. In --bench-mode ferro also hosts the realtime (socket.io),
# background workers and scheduler IN-PROCESS, so the socketio/schedule/worker/redis_* Procfile
# entries are no longer needed — the whole backend collapses to this one line.
FERRO_WEB_CMD="web: $FERRO_BIN serve --bench-mode -b 127.0.0.1:$PORT --threads $THREADS"

# Procfile entries ferro absorbs (dropped from the collapsed Procfile). `watch` is left alone —
# it's a frontend asset rebuilder, orthogonal to the runtime.
ABSORBED_RE='^(web|socketio|schedule|worker|redis_cache|redis_queue|redis_socketio):'

status() {
  local rt; rt="$(get_cfg web_runtime gunicorn)"
  echo "bench:        $BENCH_DIR"
  echo "web_runtime:  $rt"
  echo "ferro binary: $FERRO_BIN $([[ -x "$FERRO_BIN" ]] && echo '(ok)' || echo '(MISSING - build it)')"
  if [[ -f "$PROCFILE" ]]; then
    echo "Procfile ($(grep -cE '^[a-zA-Z_]+:' "$PROCFILE") process line(s)):"
    grep -E '^[a-zA-Z_]+:' "$PROCFILE" | sed 's/^/  /'
  fi
}

switch_on() {
  [[ -x "$FERRO_BIN" ]] || { echo "error: ferro binary not found/executable at: $FERRO_BIN"; echo "build it: (cd <ferro repo> && cargo build --release --bin ferro)  — or set FERRO_BIN"; exit 1; }
  if [[ -f "$PROCFILE" ]]; then
    # Back up the ENTIRE original Procfile once, so 'off' restores it byte-for-byte.
    [[ -f "$PROCFILE.ferro-orig" ]] || cp "$PROCFILE" "$PROCFILE.ferro-orig"
    # Collapse: replace the web line with the all-in-one ferro command and drop every other line
    # ferro now hosts internally (socketio/schedule/worker/redis_*). Keep anything else (e.g. watch).
    py - "$PROCFILE" "$FERRO_WEB_CMD" "$ABSORBED_RE" <<'PY'
import sys,re
p, newweb, absorbed = sys.argv[1], sys.argv[2], sys.argv[3]
absorbed_re = re.compile(absorbed)
lines = open(p).read().splitlines()
out = [newweb]
for ln in lines:
    if absorbed_re.match(ln):
        continue  # ferro hosts this in-process now
    out.append(ln)
open(p, "w").write("\n".join(out) + "\n")
PY
  fi
  set_runtime ferro
  echo "switched ON: web runtime -> ferro (backend collapsed into one process)"
  status
  echo
  echo "restart the web process: 'bench restart' (prod) or restart 'bench start' (dev)."
}

switch_off() {
  if [[ -f "$PROCFILE.ferro-orig" ]]; then
    mv "$PROCFILE.ferro-orig" "$PROCFILE"
  elif [[ -f "$PROCFILE" ]]; then
    # No backup (already off, or first run) — best-effort restore of a stock web line.
    py - "$PROCFILE" "web: bench serve  --port $PORT" <<'PY'
import sys,re
p,origweb=sys.argv[1],sys.argv[2]
lines=open(p).read().splitlines()
out=[]; done=False
for ln in lines:
    if re.match(r'^web:', ln) and not done:
        out.append(origweb); done=True
    else:
        out.append(ln)
if not done: out.insert(0,origweb)
open(p,"w").write("\n".join(out)+"\n")
PY
  fi
  # also drop the stale per-line backup from older versions of this script
  rm -f "$PROCFILE.ferro-orig-web"
  set_runtime gunicorn
  echo "switched OFF: web runtime -> gunicorn (bench serve); original Procfile restored"
  status
}

prod_patch() {
  cat <<EOF
# ---- Production switch (supervisor / systemd) ----
# Gate the program set on the common_site_config "web_runtime" flag. When ferro is on, the web
# program becomes the all-in-one command AND the socketio / worker / schedule / redis_* programs
# are dropped — ferro hosts realtime, background jobs and the scheduler in the same process.
#
#   {% if web_runtime == 'ferro' %}
#   # one program; realtime(socket.io)+workers+scheduler are in-process. No redis-server needed.
#   command=$FERRO_BIN serve --bench-mode --site {{ default_site }} -b 127.0.0.1:{{ webserver_port }} --threads {{ gunicorn_workers }}
#   # (omit the [program:...-socketio], [...-worker], [...-schedule] and the redis_* programs)
#   {% else %}
#   command={{ bench_dir }}/env/bin/gunicorn ... frappe.app:application --preload   # (existing line)
#   # (plus the existing socketio / worker / schedule / redis programs)
#   {% endif %}
#
# Then: bench setup supervisor   (or: bench setup production) && bench restart
# Flip back: set web_runtime=gunicorn and re-run the same. nginx is untouched either way.
EOF
}

case "${1:-status}" in
  on)     switch_on ;;
  off)    switch_off ;;
  status) status ;;
  prod)   prod_patch ;;
  *) echo "usage: $0 {on|off|status|prod}"; exit 2 ;;
esac
