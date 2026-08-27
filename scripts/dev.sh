#!/usr/bin/env bash
# Run the backend and the frontend dev server together. Ctrl-C stops both.
set -euo pipefail
cd "$(dirname "$0")/.."

trap 'kill 0' EXIT
cargo run --manifest-path backend/Cargo.toml -p webland-server &
(cd frontend && trunk serve) &
wait
