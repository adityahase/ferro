#!/usr/bin/env bash
# reap.sh — trigger the control plane's idle-bench reaper.
set -euo pipefail
tok=""
[ -f /opt/ferro/control/reap.token ] && tok="$(cat /opt/ferro/control/reap.token)"
exec curl -fsS -X POST -H "X-Ferro-Admin: ${tok}" http://127.0.0.1:8080/internal/reap
