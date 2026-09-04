#!/usr/bin/env bash
# Build Baffle on Linux as a fully static binary (runs anywhere, no deps).
#
# Prerequisites:
#   1. Rust:  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
#   2. musl target + tools:
#        rustup target add x86_64-unknown-linux-musl
#        # Debian/Ubuntu:  sudo apt install musl-tools
#        # Fedora:        sudo dnf install musl-gcc
#   3. PulseAudio or PipeWire runtime (pipewire-pulse), present on all
#      mainstream desktop distros.
set -euo pipefail
cd "$(dirname "$0")/.."

echo "==> Building Baffle (release, static musl)"
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl

echo
echo "==> Done: target/x86_64-unknown-linux-musl/release/baffle"
