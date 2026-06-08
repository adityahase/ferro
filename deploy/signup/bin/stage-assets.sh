#!/usr/bin/env bash
# stage-assets.sh <bench-sites-assets-dir> <out-dir>
#
# Build a SELF-CONTAINED Frappe Desk asset tree (dereferencing the `frappe -> apps/frappe/
# frappe/public` symlink) that every ferro tenant shares read-only via `ferro serve --assets`.
# We ship only what the Desk shell actually requests — the hashed dist bundles, images, icons,
# sounds and the assets.json manifests — NOT the app's node_modules / source (which is ~340 MB).
#
# Re-run this whenever the reference bench's assets are rebuilt (`bench build`), then rsync the
# out-dir to the droplet's /opt/ferro/assets.
set -euo pipefail
A="${1:?usage: stage-assets.sh <bench/sites/assets> <out-dir>}"
OUT="${2:?usage: stage-assets.sh <bench/sites/assets> <out-dir>}"

[ -f "$A/assets.json" ] || { echo "no assets.json in $A — run 'bench build' first" >&2; exit 1; }

rm -rf "$OUT"; mkdir -p "$OUT/frappe"
cp -L "$A/assets.json" "$OUT/" 2>/dev/null || true
cp -L "$A/assets-rtl.json" "$OUT/" 2>/dev/null || true
for sub in dist images icons sounds; do
  [ -e "$A/frappe/$sub" ] && cp -rL "$A/frappe/$sub" "$OUT/frappe/$sub"
done

# sanity: the desk bundle named in the manifest must exist on disk
B="$(python3 -c "import json;print(json.load(open('$OUT/assets.json'))['desk.bundle.js'])")"
if [ -f "$OUT${B#/assets}" ]; then
  echo "staged $(du -sh "$OUT" | awk '{print $1}') -> $OUT (desk bundle present)"
else
  echo "WARNING: desk bundle $B not found under $OUT" >&2
fi
