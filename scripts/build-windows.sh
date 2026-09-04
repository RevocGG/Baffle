#!/usr/bin/env bash
# Build Baffle on Windows (MSYS/Git Bash).
#
# Prerequisites:
#   Rust GNU toolchain:  rustup default stable-x86_64-pc-windows-gnu
#   MinGW-w64 binutils on PATH: gcc, as, dlltool, and windres.
#
# Optional local tool bundles may be placed under tools/localbin and
# tools/mingw64, but they are intentionally ignored by Git. build.rs uses a
# local tools/mingw-libs directory when present and otherwise relies on the
# system MinGW import libraries.
#
# build.rs embeds assets/baffle.rc (icon, version, manifest).
#
set -euo pipefail
cd "$(dirname "$0")/.."

export PATH="$PWD/tools/localbin:$PATH"

for tool in as dlltool windres; do
  if ! command -v "$tool" >/dev/null 2>&1 && ! command -v "$tool.exe" >/dev/null 2>&1; then
    echo "error: missing $tool; install MinGW-w64 binutils or place it in tools/localbin" >&2
    exit 1
  fi
done

if ! command -v gcc >/dev/null 2>&1 && ! command -v gcc.exe >/dev/null 2>&1 && [ ! -x "$PWD/tools/mingw64/bin/gcc.exe" ]; then
  echo "error: missing gcc; install MinGW-w64 or place it in tools/mingw64/bin" >&2
  exit 1
fi

echo "==> Building Baffle (release)"
cargo build --release

echo
echo "==> Done: target/release/baffle.exe"
