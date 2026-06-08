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

# Runtime tier (web_runtime, read from the tenant's config — same flag bench-ferro-switch.sh sets):
#   "ferro"  (default) — pure-Rust: Desk + app SPAs + REST, ~15-25 MB. App *.api.* methods 404.
#   "ferrod"           — the SAME ferro binary built with embedded CPython: on top of Desk + SPA +
#                        REST it also auto-routes installed apps' whitelisted methods into their REAL
#                        Python (map built on worker start). ~46-63 MB. ferro reads web_runtime
#                        itself and only boots the interpreter for "ferrod" — here we just point at
#                        the python-enabled build and put libpython + the shim/app sources on its env.
WEB_RUNTIME="$(python3 - "$SITE_DIR" <<'PY'
import json, os, sys
sd = sys.argv[1]
def load(p):
    try:
        with open(p) as f: return json.load(f)
    except Exception: return {}
cfg = {**load(os.path.join(os.path.dirname(sd), "common_site_config.json")),
       **load(os.path.join(sd, "site_config.json"))}
print(cfg.get("web_runtime", "ferro"))
PY
)"

if [ "$WEB_RUNTIME" = "ferrod" ]; then
  PY_BIN="${FERRO_RUNTIME_PY_BIN:-/opt/ferro/runtime/ferro-py}"
  if [ -x "$PY_BIN" ]; then
    RUNTIME_BIN="$PY_BIN"
    # embedded CPython needs its libpython on the loader path + the shim/app sources discoverable
    export LD_LIBRARY_PATH="${FERRO_PYLIB:-/opt/ferro/python/lib}:${LD_LIBRARY_PATH:-}"
    export FERRO_SHIM="${FERRO_SHIM:-/opt/ferro/stack/framework/shim}"
    export FERRO_REPOS="${FERRO_REPOS:-/opt/ferro/app-mirror}"
  else
    # No python-enabled build staged yet: don't hard-fail — serve the pure-Rust tier (Desk + SPA
    # shells load; app *.api.* methods 404 until ferro-py is deployed).
    echo "WARN: web_runtime=ferrod but no python-enabled ferro at $PY_BIN — falling back to pure-Rust ferro (app methods will 404)" >&2
  fi
fi

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
