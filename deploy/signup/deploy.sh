#!/usr/bin/env bash
# deploy.sh — install/refresh the Ferro signup control plane on the droplet (idempotent).
# Run on the droplet from the synced source dir:  bash /opt/ferro/signup-src/deploy.sh
set -euo pipefail
SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT=/opt/ferro

echo "==> directories"
mkdir -p "$ROOT"/{control/static,bin,tenants,logs,runtime}

echo "==> control plane"
install -m644 "$SRC/control/server.py"            "$ROOT/control/server.py"
install -m644 "$SRC/control/static/signup.html"   "$ROOT/control/static/signup.html"
install -m644 "$SRC/control/static/instance.html" "$ROOT/control/static/instance.html"

echo "==> launchers"
install -m755 "$SRC/bin/run-instance.sh"          "$ROOT/bin/run-instance.sh"
install -m755 "$SRC/bin/reap.sh"                  "$ROOT/bin/reap.sh"
install -m755 "$SRC/bin/install-wildcard-cert.sh" "$ROOT/bin/install-wildcard-cert.sh"
install -m755 "$SRC/bin/import-workspaces.py"     "$ROOT/bin/import-workspaces.py"
install -m755 "$SRC/bin/import-dashboards.py"     "$ROOT/bin/import-dashboards.py"
chmod +x "$ROOT/stack/bin/ferro" 2>/dev/null || true

echo "==> Desk preflight"
# The desk-capable runtime binary and the shared built-asset tree are shipped out-of-band
# (rsync'd straight to the droplet — they are too large / not source to live in this repo).
if [ -x "$ROOT/runtime/ferro" ]; then
  echo "    runtime: $($ROOT/runtime/ferro serve 2>&1 | head -1 >/dev/null && echo ok)  $(ls -la "$ROOT/runtime/ferro" | awk '{print $5" bytes"}')"
else
  echo "    WARNING: no desk runtime at $ROOT/runtime/ferro — tenants will fall back to the stack build (no Desk)."
fi
if [ -d "$ROOT/assets" ] && [ -f "$ROOT/assets/assets.json" ]; then
  echo "    assets:  $(du -sh "$ROOT/assets" | awk '{print $1}') at $ROOT/assets"
else
  echo "    WARNING: no asset tree at $ROOT/assets — Desk bundles will 404. Stage them (rsync) before serving Desk."
fi

echo "==> reap token"
if [ ! -f "$ROOT/control/reap.token" ]; then
  ( umask 077; head -c16 /dev/urandom | od -An -tx1 | tr -d ' \n' > "$ROOT/control/reap.token" )
fi

echo "==> systemd units"
install -m644 "$SRC/systemd/ferro-control.service"   /etc/systemd/system/ferro-control.service
install -m644 "$SRC/systemd/ferro-instance@.service" /etc/systemd/system/ferro-instance@.service
install -m644 "$SRC/systemd/ferro-reaper.service"    /etc/systemd/system/ferro-reaper.service
install -m644 "$SRC/systemd/ferro-reaper.timer"      /etc/systemd/system/ferro-reaper.timer
install -m644 "$SRC/systemd/caddy.service"           /etc/systemd/system/caddy.service

echo "==> caddy config"
mkdir -p /etc/caddy
install -m644 "$SRC/caddy/Caddyfile" /etc/caddy/Caddyfile
mkdir -p /var/lib/caddy && chown -R caddy:caddy /var/lib/caddy 2>/dev/null || true
# (re)install the wildcard cert into the caddy-readable dir if certbot has issued it
if [ -f /etc/letsencrypt/live/ferro/fullchain.pem ]; then
  "$ROOT/bin/install-wildcard-cert.sh" || echo "  (cert install skipped)"
fi

echo "==> enable + (re)start control plane and reaper"
systemctl daemon-reload
systemctl enable --now ferro-control.service
systemctl restart ferro-control.service
systemctl enable --now ferro-reaper.timer

echo "==> done. control plane:"
sleep 1
systemctl --no-pager --lines=0 status ferro-control.service | head -4 || true
curl -fsS http://127.0.0.1:8080/healthz && echo " (healthz ok)"
