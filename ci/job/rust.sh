#!/usr/bin/env bash
# Shared rust job body; workflows supply the runtime and checkout.
set -Eeuo pipefail

rustup component add rustfmt clippy
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo test --locked --test lock_process -- --ignored --test-threads=1 --nocapture
bash ci/min-rust.sh "${RUST_MIN_MODE:-check}"
