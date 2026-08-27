#!/usr/bin/env bash
# Everything CI would run.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo fmt --manifest-path backend/Cargo.toml --all --check
cargo clippy --manifest-path backend/Cargo.toml --workspace --all-targets -- -D warnings
cargo test --manifest-path backend/Cargo.toml --workspace

cargo fmt --manifest-path frontend/Cargo.toml --all --check
cargo clippy --manifest-path frontend/Cargo.toml --target wasm32-unknown-unknown --all-targets -- -D warnings
(cd frontend && trunk build)
