#!/usr/bin/env bash
# ship-runtime.sh — build the desk-capable ferro runtimes and push them to the signup droplet.
#
# Run from a dev box (this repo checked out). Produces TWO artifacts and rsyncs them to the
# droplet's out-of-band runtime dir, then restarts the per-tenant units and smoke-checks one:
#   /opt/ferro/runtime/ferro     — pure-Rust build (web_runtime=ferro tenants: Desk + SPA + REST)
#   /opt/ferro/runtime/ferro-py  — `ferro` built --features python (web_runtime=ferrod tenants:
#                                  + installed apps' whitelisted methods run their real Python)
#
# Usage:  HOST=root@ferro.x.frappe.dev bash deploy/signup/bin/ship-runtime.sh
# Env:
#   HOST          ssh target (default root@ferro.x.frappe.dev)
#   PYO3_PYTHON   embeddable CPython 3.13 (--enable-shared) for the ferro-py build
#                 (default: the pyenv 3.13.13 if present)
set -euo pipefail

HOST="${HOST:-root@ferro.x.frappe.dev}"
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
STAGE="${STAGE:-/tmp/ferro-deploy}"
PYO3_PYTHON="${PYO3_PYTHON:-$HOME/.pyenv/versions/3.13.13/bin/python3.13}"
mkdir -p "$STAGE"

cd "$REPO"

echo "==> build pure-Rust ferro"
cargo build --release
cp target/release/ferro "$STAGE/ferro"

echo "==> build ferro-py (--features python, PYO3_PYTHON=$PYO3_PYTHON)"
if [ -x "$PYO3_PYTHON" ]; then
  PYLIB="$(dirname "$(dirname "$PYO3_PYTHON")")/lib"
  PYO3_PYTHON="$PYO3_PYTHON" LD_LIBRARY_PATH="$PYLIB:${LD_LIBRARY_PATH:-}" \
    cargo build --release --features python
  cp target/release/ferro "$STAGE/ferro-py"
else
  echo "    WARNING: no embeddable 3.13 at $PYO3_PYTHON — skipping ferro-py (ferrod tenants keep the old build)"
fi

echo "==> sanity: pure-Rust ferro must NOT link libpython"
if ldd "$STAGE/ferro" 2>/dev/null | grep -qi python; then
  echo "    ERROR: $STAGE/ferro links libpython — wrong artifact" >&2; exit 1
fi

echo "==> rsync runtimes to $HOST:/opt/ferro/runtime/"
rsync -avz "$STAGE/ferro" "$HOST:/opt/ferro/runtime/ferro"
[ -f "$STAGE/ferro-py" ] && rsync -avz "$STAGE/ferro-py" "$HOST:/opt/ferro/runtime/ferro-py"

# The Python shim (frappe/erpnext compat layer the ferro-py runtime imports via FERRO_SHIM) is
# version-locked to the binary — ship it together so e.g. a new ferro_rt.insert signature and the
# shim that calls it never drift. Stale .pyc is cleared so the new sources win.
echo "==> sync shim to $HOST:/opt/ferro/stack/framework/shim/"
rsync -avz --delete --exclude='__pycache__' "$REPO/framework/shim/" "$HOST:/opt/ferro/stack/framework/shim/"
ssh "$HOST" 'find /opt/ferro/stack/framework/shim -name __pycache__ -type d -prune -exec rm -rf {} + 2>/dev/null || true'

echo "==> restart tenant units + smoke-check"
ssh "$HOST" 'set -e
  chmod +x /opt/ferro/runtime/ferro /opt/ferro/runtime/ferro-py 2>/dev/null || true
  units=$(systemctl list-units "ferro-instance@*" --no-legend --plain 2>/dev/null | awk "{print \$1}")
  if [ -n "$units" ]; then systemctl restart $units; fi
  sleep 2
  # smoke: the control plane is up and at least one tenant answers the desk shell.
  systemctl is-active ferro-control.service || true
  for u in $units; do
    sub=$(echo "$u" | sed -E "s/ferro-instance@(.*)\.service/\1/")
    port=$(grep -E "^PORT=" "/opt/ferro/tenants/$sub/instance.env" 2>/dev/null | cut -d= -f2)
    [ -n "$port" ] && echo "  $sub :$port -> $(curl -s -o /dev/null -w "%{http_code}" "http://127.0.0.1:$port/app")"
  done'

echo "==> done"
