#!/usr/bin/env bash
# min-rust.sh — install the declared minimum Rust and compile with it.
#
# usage: min-rust.sh check|build
#   check — cargo check of all targets plus the musl-target check (what pull
#           requests run; it still type-checks the musl cfg paths without
#           paying for codegen and linking)
#   build — full build of all targets plus the musl release build (what main,
#           tag, schedule and dispatch runs keep)
#
# The version comes from Cargo.toml's rust-version key — the single source;
# a missing, duplicated or malformed value fails.
set -Eeuo pipefail

mode="${1:-}"
case "${mode}" in
    check|build) ;;
    *) echo "usage: min-rust.sh check|build" >&2; exit 2 ;;
esac

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"

[ "$(grep -c '^rust-version = ' Cargo.toml)" = "1" ] \
    || { echo "min-rust: Cargo.toml must carry exactly one rust-version key" >&2; exit 1; }
declared="$(sed -n 's/^rust-version = "\([^"]*\)"$/\1/p' Cargo.toml)"
printf '%s' "${declared}" | grep -qE '^[0-9]+\.[0-9]+(\.[0-9]+)?$' \
    || { echo "min-rust: malformed rust-version '${declared}'" >&2; exit 1; }
case "${declared}" in
    *.*.*) minimum="${declared}" ;;
    *) minimum="${declared}.0" ;;
esac
echo "min-rust: declared minimum toolchain ${minimum}"
rustup toolchain install --profile minimal --target x86_64-unknown-linux-musl "${minimum}"
case "${mode}" in
    check)
        rustup run "${minimum}" cargo check --locked --all-targets
        CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUSTFLAGS="-C target-feature=+crt-static" \
            rustup run "${minimum}" cargo check --locked --target x86_64-unknown-linux-musl
        ;;
    build)
        rustup run "${minimum}" cargo build --locked --all-targets
        CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUSTFLAGS="-C target-feature=+crt-static" \
            rustup run "${minimum}" cargo build --locked --release --target x86_64-unknown-linux-musl
        ;;
esac
