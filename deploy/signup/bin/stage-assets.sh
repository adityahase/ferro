#!/usr/bin/env bash
# stage-assets.sh <bench-sites-assets-dir> <out-dir>
#
# Build a SELF-CONTAINED asset tree (dereferencing the `<app> -> apps/<app>/<app>/public` symlinks)
# that every ferro tenant shares read-only via `ferro serve --assets`. It serves two surfaces:
#   * the Frappe **Desk** shell  (frappe/{dist,images,icons,sounds} + assets.json manifests), and
#   * the **app SPAs** (crm/gameplan/helpdesk/…) whose frontends ferro mounts at /crm, /g, /helpdesk
#     — those load /assets/<app>/frontend/... bundles, so we stage every app's public/ tree too.
# We dereference the per-app symlink but never pull in node_modules / source (~340 MB) — only the
# built `public/` output that `bench build` produced.
#
# Re-run this whenever the reference bench's assets are rebuilt (`bench build`), then rsync the
# out-dir to the droplet's /opt/ferro/assets. NB: `bench build` also writes each SPA app's shell to
# apps/<app>/<app>/www/<route>.html — the app-mirror ferro reads must be that SAME built tree (sync
# the mirror from a frontend-built bench), or the SPA routes won't have a shell to serve.
set -euo pipefail
A="${1:?usage: stage-assets.sh <bench/sites/assets> <out-dir>}"
OUT="${2:?usage: stage-assets.sh <bench/sites/assets> <out-dir>}"

[ -f "$A/assets.json" ] || { echo "no assets.json in $A — run 'bench build' first" >&2; exit 1; }

rm -rf "$OUT"; mkdir -p "$OUT"
cp -L "$A/assets.json" "$OUT/" 2>/dev/null || true
cp -L "$A/assets-rtl.json" "$OUT/" 2>/dev/null || true

# Frappe (Desk): ship only the subtrees the Desk shell requests, to keep size down.
mkdir -p "$OUT/frappe"
for sub in dist images icons sounds; do
  [ -e "$A/frappe/$sub" ] && cp -rL "$A/frappe/$sub" "$OUT/frappe/$sub"
done

# App SPAs: stage every other app's built public/ tree wholesale (dereferenced), so
# /assets/<app>/frontend/... (and manifest/icons) resolve for the frappe-ui frontends.
for d in "$A"/*/; do
  name="$(basename "$d")"
  [ "$name" = "frappe" ] && continue
  [ -e "$d" ] || continue
  cp -rL "$d" "$OUT/$name" && echo "staged app assets: $name"
done

# sanity: the desk bundle named in the manifest must exist on disk
B="$(python3 -c "import json;print(json.load(open('$OUT/assets.json'))['desk.bundle.js'])")"
if [ -f "$OUT${B#/assets}" ]; then
  echo "staged $(du -sh "$OUT" | awk '{print $1}') -> $OUT (desk bundle present)"
else
  echo "WARNING: desk bundle $B not found under $OUT" >&2
fi
