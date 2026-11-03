#!/usr/bin/env bash
# Build with Docker so no Rust/C++ toolchain is needed on the machine.
# Usage: ./build.sh [cargo args...]   (default: build --release)
set -euo pipefail
cd "$(dirname "$0")"
image=imla-build:3
if ! docker image inspect "$image" >/dev/null 2>&1; then
  docker build -q -t "$image" packaging/docker >/dev/null
fi
mkdir -p .cache/cargo
args=("$@"); [ ${#args[@]} -eq 0 ] && args=(build --release)
exec docker run --rm -u "$(id -u):$(id -g)" \
  -e CARGO_HOME=/work/.cache/cargo -e HOME=/tmp \
  -v "$PWD":/work -w /work "$image" cargo "${args[@]}"
