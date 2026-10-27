# Toolchain image for cross-compiling the Windows build from Linux or macOS
# (used by `make windows-docker`).
FROM rust:1

RUN apt-get update \
    && apt-get install -y --no-install-recommends mingw-w64 \
    && rm -rf /var/lib/apt/lists/* \
    && rustup target add x86_64-pc-windows-gnu

ENV CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc
