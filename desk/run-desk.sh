#!/usr/bin/env bash
# Serve the Frappe Desk admin SPA from the pure-Rust ferro runtime (no Python).
#
#   ./run-desk.sh            # build (release) + serve on :8001 against the bench's SQLite site
#   open http://localhost:8001/app
#
# Requirements: the bench's frontend assets must be built once (`bench build --app frappe`),
# which populates <bench>/sites/assets. ferro serves those static files + computes the rest.
set -euo pipefail

FERRO_DIR="${FERRO_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"   # repo root (desk/ -> ..)
SITE=${SITE:-/home/frappe/benches/bench-cpython314/sites/mysite.sqlite}
PORT=${PORT:-8001}

echo ">> building ferro (release)..."
( cd "$FERRO_DIR" && cargo build --release --bin ferro )

# free the port if something is already bound
fuser -k "${PORT}/tcp" 2>/dev/null || true
sleep 1

echo ">> serving Desk for $SITE on http://localhost:${PORT}/app"
exec "$FERRO_DIR/target/release/ferro" serve "$SITE" --port "$PORT" --desk --dev
