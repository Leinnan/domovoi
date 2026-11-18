#!/usr/bin/env bash
# Cross-builds a headless domovoi-server for Linux from macOS, with zig as the C
# toolchain and glibc 2.34 as the floor (Fedora, Debian 12, Ubuntu 22.04, RHEL 9):
# `cargo install cargo-zigbuild`, zig on PATH, `rustup target add
# x86_64-unknown-linux-gnu` (or `TARGET=aarch64-unknown-linux-gnu`). No status
# bar icon (`--no-default-features`), so no GTK. Extra arguments go to cargo;
# prints the binary's path.
set -euo pipefail
cd "$(dirname "$0")/../.."
target="${TARGET:-x86_64-unknown-linux-gnu}"
glibc="${GLIBC:-2.34}"
# zig's linker notes a deprecated `-O1` on every link; it is harmless.
var="CARGO_TARGET_$(tr 'a-z-' 'A-Z_' <<<"$target")_RUSTFLAGS"
export "$var=${!var:-} -A linker_messages"
cargo zigbuild --release -p domovoi-server --no-default-features --target "$target.$glibc" "$@" >&2
echo "target/$target/release/domovoi-server"
