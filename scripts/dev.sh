#!/usr/bin/env bash
# Run the backend and the frontend dev server together. Ctrl-C stops both.
#
# The backend serves the Webland protocol over WebSocket on 127.0.0.1:9001, which
# matches `BACKEND` in frontend/src/desktop, and runs headless (WEBLAND_HEADLESS)
# so the browser is the only display. To see an actual surface, point a Wayland
# client at it, e.g.:  WEBLAND_SPAWN=kitty ./scripts/dev.sh
# (unset WEBLAND_HEADLESS to also get a local debug window.)
#
# WEBLAND_PORT moves the frontend off 3030 — e.g. onto the homelab's web-shell
# port to try it where a terminal already lives. Localhost either way.
set -euo pipefail
cd "$(dirname "$0")/.."

trap 'kill 0' EXIT
WEBLAND_WS="${WEBLAND_WS:-127.0.0.1:9001}" \
WEBLAND_HEADLESS="${WEBLAND_HEADLESS:-1}" \
  cargo run --manifest-path backend/Cargo.toml -p webland-server &
# Trunk has no flag to turn off its emoji, so they get stripped on the way past.
(cd frontend && trunk serve --color always --port "${WEBLAND_PORT:-3030}" 2>&1 \
  | perl -CSD -pe '$|=1; s/[\x{1F300}-\x{1FAFF}\x{2600}-\x{27BF}\x{2B00}-\x{2BFF}\x{FE0F}]+\s*//g') &
wait
