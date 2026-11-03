#!/usr/bin/env bash
# Build with Docker so no Rust toolchain is needed on the machine.
# Usage: ./build.sh [cargo args...]   (default: build --release)
set -euo pipefail
cd "$(dirname "$0")"
mkdir -p .cache/cargo-registry
args=("$@"); [ ${#args[@]} -eq 0 ] && args=(build --release)
exec docker run --rm -u "$(id -u):$(id -g)" \
  -e CARGO_HOME=/work/.cache/cargo -e HOME=/tmp \
  -v "$PWD":/work -w /work rust:1-slim-bookworm cargo "${args[@]}"
