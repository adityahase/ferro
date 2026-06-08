#!/usr/bin/env bash
# bootstrap.sh — one command from a clean checkout to a running Ferro serving the apps.
#
# Does, in order:
#   1. (optional) scripts/setup.sh         — install toolchain deps      (--no-setup to skip)
#   2. ferro build                          — compile the runtime
#   3. ferro init <forge>                   — create the workspace
#   4. ferro new-site <site>                — site from the frappe-core seed
#   5. ferro get-app / install-app <apps>   — fetch + materialise each app's schema
#   6. ferro populate                       — representative demo rows
#   7. prints how to serve / run a frontend / verify
#
# Usage:  scripts/bootstrap.sh [--forge DIR] [--site NAME] [--apps a,b,c]
#                              [--no-setup] [--no-build] [--no-populate]
#                              [--runtime ferrod|ferro|native]
set -euo pipefail

FORGE_DIR="forge"; SITE="dev.localhost"; APPS="crm,helpdesk,gameplan,hrms,erpnext"
DO_SETUP=1; DO_POP=1; DO_BUILD=1; RUNTIME="ferrod"
while [ $# -gt 0 ]; do
  case "$1" in
    --forge) shift; FORGE_DIR="$1" ;;
    --site)  shift; SITE="$1" ;;
    --apps)  shift; APPS="$1" ;;
    --runtime) shift; RUNTIME="$1" ;;
    --no-setup) DO_SETUP=0 ;;
    --no-populate) DO_POP=0 ;;
    --no-build) DO_BUILD=0 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
  shift
done

c() { [ -t 2 ] && printf "\033[%sm%s\033[0m" "$1" "$2" || printf "%s" "$2"; }
step() { printf "\n%s %s\n" "$(c 36 '====>')" "$(c 1 "$*")" >&2; }
good() { printf "%s %s\n" "$(c 32 ' ok')" "$*" >&2; }
die()  { printf "%s %s\n" "$(c 31 'err')" "$*" >&2; exit 1; }

HOME_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"  # repo root (cli/scripts/ -> ../..)
export FERRO_HOME="$HOME_DIR"
FERRO="$HOME_DIR/cli/ferro"
[ -x "$FERRO" ] || die "CLI not found/executable at $FERRO"

t0=$(date +%s)

if [ "$DO_SETUP" = 1 ]; then
  step "1/6  install dependencies (scripts/setup.sh)"
  bash "$HOME_DIR/cli/scripts/setup.sh" --yes || die "setup failed (re-run with --no-setup if deps are already present)"
else
  step "1/6  skipping dependency install (--no-setup)"
fi
# make freshly-installed rust/pyenv visible to this shell
. "$HOME/.cargo/env" 2>/dev/null || true
export PATH="$HOME/.pyenv/bin:$HOME/.pyenv/shims:$PATH"

step "2/6  build the runtime ($RUNTIME)"
if [ "$DO_BUILD" = 0 ]; then
  good "skipping build (--no-build); using existing binaries"
elif ! "$FERRO" build "$RUNTIME"; then
  if [ "$RUNTIME" = "ferrod" ]; then
    printf "%s %s\n" "$(c 33 '  !')" "ferrod build failed — falling back to pure-ferro (no embedded Python)" >&2
    RUNTIME="ferro"; "$FERRO" build ferro || die "runtime build failed"
  else
    die "runtime build failed"
  fi
fi
good "runtime built"

step "3/6  create forge -> $FORGE_DIR"
if [ -f "$FORGE_DIR/ferro.json" ]; then good "forge already exists"; else "$FERRO" init "$FORGE_DIR"; fi
cd "$FORGE_DIR"
export FERRO_FORGE="$PWD"

step "4/6  create site -> $SITE"
if [ -d "sites/$SITE" ]; then good "site already exists"; else "$FERRO" new-site "$SITE"; fi

step "5/6  install apps -> $APPS"
IFS=',' read -ra APP_LIST <<< "$APPS"
for app in "${APP_LIST[@]}"; do
  [ -d "apps/$app" ] || "$FERRO" get-app "$app" || die "get-app $app failed"
  "$FERRO" install-app "$app" --site "$SITE" || die "install-app $app failed"
done

if [ "$DO_POP" = 1 ]; then
  step "6/6  populate demo rows"
  "$FERRO" populate --site "$SITE" --rows 3000 || true
else
  step "6/6  skipping demo data (--no-populate)"
fi

dt=$(( $(date +%s) - t0 ))
echo
good "bootstrap complete in ${dt}s — forge: $PWD"
cat >&2 <<EOF

  $(c 1 'Serve the REST API:')
      cd $PWD
      ferro serve --runtime $RUNTIME --apps $APPS --load lazy
      curl http://127.0.0.1:8000/api/resource/CRM%20Deal?limit_page_length=2

  $(c 1 'Run an app frontend (needs node):')
      ferro dev crm                 # vite dev server, proxied to ferro

  $(c 1 'Measure memory / prove the apps run:')
      ferro measure --apps $APPS --load lazy
      ferro verify
EOF
