# Build environment: Rust plus the C/C++ toolchain whisper.cpp (whisper-rs) needs.
FROM rust:1-slim-trixie
RUN apt-get update \
 && apt-get install -y --no-install-recommends cmake clang libclang-dev g++ make pkg-config git libvulkan-dev glslc \
 && rm -rf /var/lib/apt/lists/*
