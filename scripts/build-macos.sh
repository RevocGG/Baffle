#!/usr/bin/env bash
# Build Baffle on macOS.
#
# Prerequisites:
#   1. Rust:  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
#   2. Xcode Command Line Tools:  xcode-select --install
#
# First run: macOS will prompt for Screen Recording / Audio permission —
# grant it once; the app remembers.
set -euo pipefail
cd "$(dirname "$0")/.."

echo "==> Building Baffle (release, universal binary)"
rustup target add aarch64-apple-darwin x86_64-apple-darwin
cargo build --release --target aarch64-apple-darwin
cargo build --release --target x86_64-apple-darwin

BIN=target/aarch64-apple-darwin/release/baffle
BIN2=target/x86_64-apple-darwin/release/baffle
lipo -create -output target/baffle-universal "$BIN" "$BIN2" 2>/dev/null || {
  echo "==> lipo failed (one arch missing?), keeping single-arch binary"; exit 0; }

echo
echo "==> Done: target/baffle-universal"
