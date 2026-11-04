# syntax=docker/dockerfile:1
# MagicDAB for Linux on 64-bit ARM (Raspberry Pi 3/4/5 with a 64-bit OS),
# cross-compiled on an x86_64 host: one program with the web interface inside
# and the codecs linked in. No RTL-SDR library is bundled (the system's
# librtlsdr is loaded when present); libusb-1.0 is needed for the DTR-100.
# Built on Ubuntu 22.04: runs with glibc 2.35 or newer. Use build/linux-arm64.sh.

# ---- web interface (built into the program) ----
FROM node:20-trixie-slim AS web
WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY web/ ./
# (without the source maps of web/vite.config.ts: nothing in the package loads them)
RUN npm run build && find dist -name '*.map' -delete

# ---- cross toolchain and codecs ----
FROM ubuntu:22.04 AS toolchain
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends build-essential cmake curl ca-certificates pkg-config \
       gcc-aarch64-linux-gnu g++-aarch64-linux-gnu libc6-dev-arm64-cross binutils-aarch64-linux-gnu \
    && rm -rf /var/lib/apt/lists/*
RUN curl -fsSL --retry 5 --retry-all-errors https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable --target aarch64-unknown-linux-gnu
ENV PATH=/root/.cargo/bin:$PATH
ARG OPUS_VERSION=1.5.2
ARG FAAD_VERSION=2.11.2
# codecs as static libraries for aarch64
RUN curl -fsSL --retry 5 --retry-all-errors https://github.com/xiph/opus/releases/download/v${OPUS_VERSION}/opus-${OPUS_VERSION}.tar.gz | tar xz -C /tmp \
    && cmake -S /tmp/opus-${OPUS_VERSION} -B /tmp/opus -DCMAKE_SYSTEM_NAME=Linux -DCMAKE_SYSTEM_PROCESSOR=aarch64 \
       -DCMAKE_C_COMPILER=aarch64-linux-gnu-gcc -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=OFF \
       -DCMAKE_POSITION_INDEPENDENT_CODE=ON -DOPUS_BUILD_TESTING=OFF -DOPUS_BUILD_PROGRAMS=OFF -DCMAKE_INSTALL_PREFIX=/opt/static \
    && cmake --build /tmp/opus -j1 && cmake --install /tmp/opus
RUN curl -fsSL --retry 5 --retry-all-errors https://github.com/knik0/faad2/archive/refs/tags/${FAAD_VERSION}.tar.gz | tar xz -C /tmp \
    && cmake -S /tmp/faad2-${FAAD_VERSION} -B /tmp/faad -DCMAKE_SYSTEM_NAME=Linux -DCMAKE_SYSTEM_PROCESSOR=aarch64 \
       -DCMAKE_C_COMPILER=aarch64-linux-gnu-gcc -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=OFF \
       -DCMAKE_POSITION_INDEPENDENT_CODE=ON -DFAAD_BUILD_CLI=OFF -DFAAD_APPLY_DRC=OFF -DCMAKE_INSTALL_PREFIX=/opt/static \
    && cmake --build /tmp/faad -j1 && cmake --install /tmp/faad
ENV FAAD_LIB_DIR=/opt/static/lib OPUS_LIB_DIR=/opt/static/lib \
    CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc CXX_aarch64_unknown_linux_gnu=aarch64-linux-gnu-g++ \
    AR_aarch64_unknown_linux_gnu=aarch64-linux-gnu-ar CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc

# ---- program ----
FROM toolchain AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY third_party ./third_party
COPY --from=web /web/dist ./web/dist
ARG VERSION=0.1.0
ARG UPDATE_URL=
# the address of the source code (AGPL section 13)
ARG SOURCE_URL=
# fewer parallel jobs on a busy host (e.g. next to a running receiver)
ARG CARGO_BUILD_JOBS=
ENV MAGICDAB_VERSION=${VERSION} MAGICDAB_UPDATE_URL=${UPDATE_URL} MAGICDAB_SOURCE_URL=${SOURCE_URL}
RUN --mount=type=cache,target=/root/.cargo/registry --mount=type=cache,target=/src/target \
    env -u CARGO_BUILD_JOBS cargo build ${CARGO_BUILD_JOBS:+-j $CARGO_BUILD_JOBS} --release -p magicdab --features embed-web --target aarch64-unknown-linux-gnu \
    && mkdir -p /out \
    && cp target/aarch64-unknown-linux-gnu/release/magicdab /out/ \
    && aarch64-linux-gnu-strip /out/magicdab \
    && aarch64-linux-gnu-objdump -p /out/magicdab | grep -E "NEEDED|RPATH|RUNPATH" > /out/needed.txt \
    && aarch64-linux-gnu-objdump -T /out/magicdab | grep -o "GLIBC_[0-9.]*" | sort -uV | tail -1 >> /out/needed.txt
COPY LICENSE LICENSE-EXCEPTION.txt THIRD-PARTY.md /out/

FROM scratch
COPY --from=build /out /
