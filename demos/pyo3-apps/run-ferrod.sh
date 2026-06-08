#!/bin/bash
# run-ferrod.sh — launch ferrod with the recommended memory-minimising configuration.
#
#   ./run-ferrod.sh <subcommand> [args...]
#       measure  <site> [--apps a,b] [--load all|lazy|none]
#       serve    <site> [--port N] [--threads N] [--apps ...] [--load all|lazy]
#       loadtest <site> [--threads N] [--rounds N] [--apps ...] [--load all|lazy]
#       request  <site> METHOD url [body]
#
# Recommended allocator config: jemalloc with immediate page decay (returns freed pages to the
# OS), single arena (no per-thread fragmentation), no thread cache. Plus a small SQLite page
# cache and CPython's debug-range tables dropped (set inside ferrod). This is the configuration
# under which the all-apps-eager worst case stays within 64 MB.
set -u
# Repo root, two levels up from demos/pyo3-apps/ (override with $FERRO_DIR).
FERRO_DIR="${FERRO_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
PYLIB=/home/frappe/.pyenv/versions/3.13.13/lib
JEMALLOC=/usr/lib/x86_64-linux-gnu/libjemalloc.so.2

export LD_LIBRARY_PATH="$PYLIB${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
# The embedded-Python `frappe` shim (deduplicated — the single copy lives in framework/).
export FERRO_SHIM="${FERRO_SHIM:-$FERRO_DIR/framework/shim}"
# App controller sources are a bring-your-own input; clone into the path below or set $FERRO_REPOS.
export FERRO_REPOS="${FERRO_REPOS:-$FERRO_DIR/docs/investigations/apps-64mb/repos}"
export FERRO_CACHE_KB="${FERRO_CACHE_KB:-256}"
export MALLOC_CONF="${MALLOC_CONF:-narenas:1,dirty_decay_ms:0,muzzy_decay_ms:0,tcache:false,background_thread:true}"

PRELOAD=""
[ -f "$JEMALLOC" ] && PRELOAD="$JEMALLOC"

exec env LD_PRELOAD="$PRELOAD" "$FERRO_DIR/target/release/ferrod" "$@"
