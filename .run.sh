#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"

usage() {
  cat <<'USAGE'
./.run.sh [command] [-- server args...]

  dev       (default) backend + trunk serve, debug
  build     compile backend and frontend in release, in parallel
  release   build, then run the release binary + trunk serve --release
  run       run existing release artifacts without rebuilding
  check     fmt, clippy and tests for both halves, plus a wasm build

env: WEBLAND_WS (127.0.0.1:9001), WEBLAND_PORT (3030), WEBLAND_HEADLESS (1),
     WEBLAND_SPAWN, WEBLAND_SIZE, WEBLAND_RENDER_NODE, WEBLAND_BITRATE, RUST_LOG
USAGE
}

# Trunk has no flag to turn off its emoji, so they get stripped on the way past.
strip_emoji() {
  perl -CSD -pe '$|=1; s/[\x{1F300}-\x{1FAFF}\x{2600}-\x{27BF}\x{2B00}-\x{2BFF}\x{FE0F}]+\s*//g'
}

cmd="${1:-dev}"
[ $# -gt 0 ] && shift
[ "${1:-}" = "--" ] && shift

export WEBLAND_WS="${WEBLAND_WS:-127.0.0.1:9001}"
export WEBLAND_HEADLESS="${WEBLAND_HEADLESS:-1}"
PORT="${WEBLAND_PORT:-3030}"
SERVER_BIN="${CARGO_TARGET_DIR:-backend/target}/release/webland-server"

build_release() {
  cargo build --release --manifest-path backend/Cargo.toml -p webland-server &
  # From inside frontend/, not by --manifest-path: cargo reads .cargo/config.toml
  # from the working directory, and that is where the WebCodecs cfg lives.
  (cd frontend && trunk build --release 2>&1 | strip_emoji) &
  wait
}

run_release() {
  [ -x "$SERVER_BIN" ] && [ -f frontend/dist/index.html ] || {
    echo "no release artifacts; run './.run.sh release' first" >&2
    exit 1
  }
  echo "frontend http://127.0.0.1:$PORT   backend $WEBLAND_WS"
  trap 'kill 0' EXIT INT TERM
  "$SERVER_BIN" "$@" &
  (cd frontend && trunk serve --release --no-autoreload --color always --port "$PORT" 2>&1 | strip_emoji) &
  wait -n
}

case "$cmd" in
  dev)
    trap 'kill 0' EXIT INT TERM
    cargo run --manifest-path backend/Cargo.toml -p webland-server -- "$@" &
    (cd frontend && trunk serve --color always --port "$PORT" 2>&1 | strip_emoji) &
    wait
    ;;
  build)   build_release ;;
  release) build_release; run_release "$@" ;;
  run)     run_release "$@" ;;
  check)
    cargo fmt --manifest-path backend/Cargo.toml --all --check
    cargo clippy --manifest-path backend/Cargo.toml --workspace --all-targets -- -D warnings
    cargo test --manifest-path backend/Cargo.toml --workspace
    cd frontend
    cargo fmt --all --check
    cargo clippy --target wasm32-unknown-unknown --all-targets -- -D warnings
    trunk build
    ;;
  -h|--help|help) usage ;;
  *) usage >&2; exit 1 ;;
esac
