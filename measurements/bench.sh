#!/bin/bash
# Usage: bench.sh <port> <threads> <meta-cap> <rounds> [env assignments...]
# Starts ferro serve with given config + env, runs the load test, prints peak mem, stops it.
set -u
FERRO=/home/frappe/ferro/target/release/ferro
SITE=/home/frappe/benches/bench-cpython314/sites/mysite.sqlite
PY=/home/frappe/benches/bench-cpython314/env/bin/python
LOAD=/home/frappe/ferro/measurements/loadtest.py

PORT=${1:-18080}; THREADS=${2:-4}; CAP=${3:-1024}; ROUNDS=${4:-8}; shift 4 || true
ENVARS="$*"

TOK=$(timeout 10 "$FERRO" provision-key "$SITE" Administrator | grep -oP 'token \K[^ ]+')
LOG=/tmp/ferro_bench_$PORT.log
# shellcheck disable=SC2086
env $ENVARS setsid "$FERRO" serve "$SITE" --port "$PORT" --threads "$THREADS" \
    --default-user Administrator --meta-cap "$CAP" >"$LOG" 2>&1 &
disown
# wait for bind
for i in $(seq 1 100); do
  curl -s -m 2 "http://127.0.0.1:$PORT/api/method/ping" >/dev/null 2>&1 && break
done
SRV=$(pgrep -f "release/ferro serve.*--port $PORT" | head -1)
if [ -z "$SRV" ]; then echo "FAILED to start (log below):"; cat "$LOG"; exit 1; fi
IDLE=$(grep -E '^Rss:' /proc/$SRV/smaps_rollup | awk '{print $2}')
echo "config: threads=$THREADS cap=$CAP rounds=$ROUNDS env=[$ENVARS] pid=$SRV idle_rss=${IDLE}kB"
"$PY" "$LOAD" "$PORT" "$TOK" "$ROUNDS" "$SRV"
kill -9 "$SRV" 2>/dev/null
