#!/bin/bash
# bench.sh — canonical memory matrix for "Frappe apps on ferro". Writes results to stdout.
set -u
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"   # repo root
cd "$ROOT"
export LD_LIBRARY_PATH=/home/frappe/.pyenv/versions/3.13.13/lib
export FERRO_SHIM="${FERRO_SHIM:-$ROOT/framework/shim}"
export FERRO_REPOS="${FERRO_REPOS:-$ROOT/docs/investigations/apps-64mb/repos}"
export FERRO_CACHE_KB=256
SITE="$ROOT/demos/pyo3-apps/site"
ALL="crm,helpdesk,gameplan,hrms,erpnext"
JE=/usr/lib/x86_64-linux-gnu/libjemalloc.so.2
MC="narenas:1,dirty_decay_ms:0,muzzy_decay_ms:0,tcache:false,background_thread:true"
FERROD=./target/release/ferrod
run() { LD_PRELOAD=$JE MALLOC_CONF="$MC" $FERROD "$@" 2>/dev/null; }

echo "############################################################"
echo "# ferro apps memory matrix  ($(date -u +%Y-%m-%dT%H:%M:%SZ))"
echo "# embedded CPython 3.13.13 + jemalloc(decay0,narenas1) + 256KB cache"
echo "############################################################"

echo; echo "== pure-Rust ferro (data plane only, no Python), idle 4 threads =="
"$ROOT/measurements/bench.sh" 19099 4 1024 1 2>/dev/null | grep -E "idle_rss|peak" || \
  { ./target/release/ferro "$SITE" >/dev/null; echo "  (smoke ok)"; }

echo; echo "== ferrod IDLE (post-boot), per-app eager + all eager + all lazy =="
printf "%-26s %8s %8s %8s\n" "config" "RSS" "PSS" "USS"
for app in crm helpdesk gameplan hrms erpnext; do
  line=$(run measure "$SITE" --apps $app --load all | grep -E "^(RSS|PSS|USS)")
  r=$(echo "$line"|awk '/RSS/{print $3}'); p=$(echo "$line"|awk '/PSS/{print $3}'); u=$(echo "$line"|awk '/USS/{print $3}')
  printf "%-26s %8s %8s %8s\n" "$app (eager)" "$r" "$p" "$u"
done
for cfg in "all|$ALL" "lazy|$ALL"; do
  mode=${cfg%%|*}; apps=${cfg##*|}
  line=$(run measure "$SITE" --apps $apps --load $mode | grep -E "^(RSS|PSS|USS)")
  r=$(echo "$line"|awk '/RSS/{print $3}'); p=$(echo "$line"|awk '/PSS/{print $3}'); u=$(echo "$line"|awk '/USS/{print $3}')
  printf "%-26s %8s %8s %8s\n" "ALL 5 apps ($mode)" "$r" "$p" "$u"
done

echo; echo "== ferrod UNDER LOAD (peak), all 5 apps, eager vs lazy, by threads =="
echo "   (reads = 20 rows x ALL columns against populated tables; writes = CRM Deal[+children] + ToDo)"
printf "%-26s %8s %8s %8s %6s\n" "config" "peakRSS" "peakPSS" "peakUSS" "req/s"
for mode in all lazy; do
  for T in 1 2 4 8; do
    out=$(run loadtest "$SITE" --apps $ALL --load $mode --threads $T --rounds 8)
    pkline=$(echo "$out" | grep PEAK)
    r=$(echo "$pkline" | grep -oE 'RSS=[0-9.]+' | cut -d= -f2)
    p=$(echo "$pkline" | grep -oE 'PSS=[0-9.]+' | cut -d= -f2)
    u=$(echo "$pkline" | grep -oE 'USS=[0-9.]+' | cut -d= -f2)
    rs=$(echo "$out" | grep -oE '[0-9]+ req/s' | grep -oE '^[0-9]+')
    printf "%-26s %8s %8s %8s %6s\n" "ALL ($mode, ${T}T)" "$r" "$p" "$u" "$rs"
  done
done
echo; echo "(64 MB target — compare every number above; under-load = peak during a write-storm)"
