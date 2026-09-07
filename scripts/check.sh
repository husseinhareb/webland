#!/usr/bin/env bash
# Everything CI would run.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo fmt --manifest-path backend/Cargo.toml --all --check
cargo clippy --manifest-path backend/Cargo.toml --workspace --all-targets -- -D warnings
cargo test --manifest-path backend/Cargo.toml --workspace

# From inside frontend/, not by --manifest-path: cargo reads .cargo/config.toml
# from the working directory, and that is where the WebCodecs cfg lives.
(
  cd frontend
  cargo fmt --all --check
  cargo clippy --target wasm32-unknown-unknown --all-targets -- -D warnings
  trunk build
)
