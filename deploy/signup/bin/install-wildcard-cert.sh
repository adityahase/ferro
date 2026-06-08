#!/usr/bin/env bash
# install-wildcard-cert.sh — copy the certbot wildcard cert to a caddy-readable location
# and reload Caddy. Run on first setup and as a certbot deploy hook on every renewal.
set -euo pipefail
src=/etc/letsencrypt/live/ferro
dst=/etc/caddy/wildcard
[ -f "$src/fullchain.pem" ] || { echo "no cert at $src" >&2; exit 1; }
mkdir -p "$dst"
install -m640 -o caddy -g caddy "$src/fullchain.pem" "$dst/fullchain.pem"
install -m640 -o caddy -g caddy "$src/privkey.pem"   "$dst/privkey.pem"
chgrp caddy "$dst" 2>/dev/null || true
chmod 750 "$dst"
systemctl reload caddy 2>/dev/null || systemctl restart caddy || true
echo "wildcard cert installed to $dst and Caddy reloaded"
