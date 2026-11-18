#!/usr/bin/env bash
# Builds the Rust conformance server and runs the Effect client against it.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build -q -p effect-rpc --example conformance_server
log=$(mktemp)
./target/debug/examples/conformance_server 0 >"$log" &
pid=$!
trap 'kill $pid 2>/dev/null' EXIT
for _ in $(seq 50); do grep -q LISTENING "$log" && break; sleep 0.1; done
port=$(awk '/LISTENING/{print $2}' "$log")
node conformance/effect-client.ts "ws://127.0.0.1:${port}/ws"
