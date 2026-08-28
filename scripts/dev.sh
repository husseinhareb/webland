#!/usr/bin/env bash
# Run the backend and the frontend dev server together. Ctrl-C stops both.
#
# The backend serves the Webland protocol over WebSocket on 127.0.0.1:9001, which
# matches `BACKEND` in frontend/src/desktop, and runs headless (WEBLAND_HEADLESS)
# so the browser is the only display. To see an actual surface, point a Wayland
# client at it, e.g.:  WEBLAND_SPAWN=kitty ./scripts/dev.sh
# (unset WEBLAND_HEADLESS to also get a local debug window.)
set -euo pipefail
cd "$(dirname "$0")/.."

trap 'kill 0' EXIT
WEBLAND_WS="${WEBLAND_WS:-127.0.0.1:9001}" \
WEBLAND_HEADLESS="${WEBLAND_HEADLESS:-1}" \
  cargo run --manifest-path backend/Cargo.toml -p webland-server &
(cd frontend && trunk serve) &
wait
