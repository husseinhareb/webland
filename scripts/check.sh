#!/usr/bin/env bash
# Everything CI would run.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo fmt --manifest-path backend/Cargo.toml --all --check
cargo clippy --manifest-path backend/Cargo.toml --workspace --all-targets -- -D warnings
cargo test --manifest-path backend/Cargo.toml --workspace

npm --prefix frontend run check
npm --prefix frontend run build
