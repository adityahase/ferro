#!/usr/bin/env bash
# build-spa-apps.sh — build the installed frappe-ui app frontends (crm/gameplan/helpdesk) in the
# shared app-mirror and stage their assets so ferro can serve each SPA at its route (/crm, /g,
# /helpdesk). Run on the droplet after the app-mirror is synced or an app is updated; the output is
# shared read-only by every tenant (ferro serves the www shell + /assets/<app>/...).
#
# Each app's vite build wants a Frappe *bench* context that the bare mirror lacks, so we supply the
# two things the builds reference out of tree:
#   * a stub sites/common_site_config.json (gameplan's socket.js imports socketio_port from it), and
#   * frappe/frappe/public/js/lib/posthog.js (helpdesk's telemetry.ts imports it).
# We also pass an explicit --base for the apps whose plugin can't infer it outside a bench.
#
# Prereqs: node on PATH (e.g. /opt/node/bin), the python-enabled `ferro-py` runtime + shim deployed
# (see run-instance.sh / deploy.sh) so the served SPA's /api/method/<app>.* calls actually work.
set -u
export PATH=/opt/node/bin:$PATH

MIRROR="${FERRO_APP_MIRROR:-/opt/ferro/app-mirror}"
ASSETS="${FERRO_ASSETS:-/opt/ferro/assets}"
SITES="${FERRO_SITES:-/opt/ferro/sites}"
LOGS="${FERRO_LOG_DIR:-/opt/ferro/logs}"
mkdir -p "$LOGS"

# 1. Build-context shims the app vite builds reference relative to a bench root.
mkdir -p "$SITES"
[ -f "$SITES/common_site_config.json" ] || \
  printf '{\n "socketio_port": 9000,\n "developer_mode": 0,\n "default_site": ""\n}\n' > "$SITES/common_site_config.json"
# helpdesk telemetry imports frappe's posthog.js (../../../frappe/...); provide a no-op if absent.
PH="$MIRROR/frappe/frappe/public/js/lib/posthog.js"
if [ ! -f "$PH" ]; then
  mkdir -p "$(dirname "$PH")"
  printf 'export default { init(){}, capture(){}, identify(){}, reset(){} };\n' > "$PH"
fi

stage_public() {  # <app>  — mirror the app package's built public/ into /assets/<app>
  local app="$1" pub="$MIRROR/$app/$app/public"
  rm -rf "${ASSETS:?}/$app"; mkdir -p "$ASSETS/$app"
  cp -rL "$pub"/* "$ASSETS/$app/" 2>/dev/null
}

build_app() {  # <app> <frontend-subdir> <vite-base> <route> [dist-relative]
  local app="$1" sub="$2" base="$3" route="$4" dist="${5:-}"
  local dir="$MIRROR/$app/$sub"
  [ -d "$dir" ] || { echo "  [$app] no frontend at $dir — skip"; return; }
  echo "===== build $app ($dir) ====="
  ( cd "$dir" && yarn install >"$LOGS/fe-$app-install.log" 2>&1 ) || { echo "  [$app] install failed (see $LOGS/fe-$app-install.log)"; return; }
  if [ -n "$base" ]; then
    ( cd "$dir" && yarn vite build --base="$base" >"$LOGS/fe-$app-build.log" 2>&1 )
  else
    ( cd "$dir" && yarn build >"$LOGS/fe-$app-build.log" 2>&1 )
  fi
  local rc=$?
  [ $rc -eq 0 ] || { echo "  [$app] build rc=$rc (see $LOGS/fe-$app-build.log)"; return; }
  # apps whose buildConfig writes www/<route>.html + public/frontend themselves need no dist copy;
  # gameplan (plain `dist/`) does.
  if [ -n "$dist" ]; then
    local d="$dir/$dist"
    cp "$d/index.html" "$MIRROR/$app/$app/www/$route.html"
    rm -rf "$MIRROR/$app/$app/public/frontend"; cp -r "$d" "$MIRROR/$app/$app/public/frontend"
  fi
  stage_public "$app"
  echo "  [$app] staged -> $ASSETS/$app  (route /$route)"
}

# crm + helpdesk configure their own outDir/indexHtmlPath via the frappeui buildConfig; gameplan
# emits a plain dist/ that we relocate.
build_app gameplan frontend /assets/gameplan/frontend/ g    dist
build_app crm      frontend /assets/crm/frontend/      crm
build_app helpdesk desk     ""                          helpdesk     # buildConfig sets base + www

echo "done. restart tenants (or let them wake) to pick up the built SPAs."
