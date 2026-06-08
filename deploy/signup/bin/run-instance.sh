#!/usr/bin/env bash
# run-instance.sh <sub> — launch one tenant's ferro backend in **Desk mode** from its
# instance.env. Invoked by the systemd template unit ferro-instance@<sub>.service.
#
# The tenant is served by the desk-capable `ferro` runtime: it serves the full Frappe Desk
# SPA (HTML shell + assets + desk.* methods) plus the REST API against the tenant's SQLite
# site. Desk assets are SHARED, read-only across every tenant (one built tree), passed via
# --assets so we never rebuild or copy bundles per tenant.
set -euo pipefail

sub="${1:?usage: run-instance.sh <sub>}"
forge="/opt/ferro/tenants/$sub"
[ -f "$forge/instance.env" ] || { echo "no instance.env for $sub" >&2; exit 1; }

set -a
# shellcheck disable=SC1090
. "$forge/instance.env"
set +a

# The signup-owned, desk-capable runtime binary (shipped by deploy.sh / rsync), with a
# fallback to the vendored ferro runtime release build.
RUNTIME_BIN="${FERRO_RUNTIME_BIN:-/opt/ferro/runtime/ferro}"
[ -x "$RUNTIME_BIN" ] || RUNTIME_BIN="/opt/ferro/stack/runtime/target/release/ferro"

ASSETS="${FERRO_ASSETS:-/opt/ferro/assets}"
THREADS="${FERRO_THREADS:-4}"
SITE_DIR="$forge/sites/$SITE"

desk_args=()
if [ -d "$ASSETS" ]; then
  desk_args+=(--desk --assets "$ASSETS")
else
  # Without a built asset tree we can still serve the REST API (Desk shell would 404 its
  # bundles); log loudly so the operator stages assets.
  echo "WARN: no asset tree at $ASSETS — serving REST only (no Desk)" >&2
fi

exec "$RUNTIME_BIN" serve "$SITE_DIR" \
  -b "127.0.0.1:$PORT" --threads "$THREADS" \
  --default-user Administrator "${desk_args[@]}"
