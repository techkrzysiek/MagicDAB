# syntax=docker/dockerfile:1
# MagicDAB for Linux, for the architecture of the build host (x86_64 or
# aarch64): one program with the web interface inside and the codecs linked
# in, and the RTL-SDR library next to it. Built on Ubuntu 22.04, so it runs
# with glibc 2.35 or newer (Ubuntu 22.04+, Debian 12+, Raspberry Pi OS
# bookworm+, Fedora 36+). Use build/linux.sh (zip and update manifest).

# ---- web interface (built into the program) ----
FROM node:20-trixie-slim AS web
WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY web/ ./
# (without the source maps of web/vite.config.ts: nothing in the package loads them)
RUN npm run build && find dist -name '*.map' -delete

# ---- toolchain, codecs, RTL-SDR library ----
FROM ubuntu:22.04 AS toolchain
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends build-essential cmake curl ca-certificates pkg-config \
       libusb-1.0-0-dev \
    && rm -rf /var/lib/apt/lists/*
RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable
ENV PATH=/root/.cargo/bin:$PATH
# codecs as static libraries (no codec packages needed where it runs)
ARG OPUS_VERSION=1.5.2
ARG FAAD_VERSION=2.11.2
RUN curl -fsSL https://downloads.xiph.org/releases/opus/opus-${OPUS_VERSION}.tar.gz | tar xz -C /tmp \
    && cmake -S /tmp/opus-${OPUS_VERSION} -B /tmp/opus -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=OFF \
       -DCMAKE_POSITION_INDEPENDENT_CODE=ON -DOPUS_BUILD_TESTING=OFF -DOPUS_BUILD_PROGRAMS=OFF -DCMAKE_INSTALL_PREFIX=/opt/static \
    && cmake --build /tmp/opus -j"$(nproc)" && cmake --install /tmp/opus
RUN curl -fsSL https://github.com/knik0/faad2/archive/refs/tags/${FAAD_VERSION}.tar.gz | tar xz -C /tmp \
    && cmake -S /tmp/faad2-${FAAD_VERSION} -B /tmp/faad -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=OFF \
       -DCMAKE_POSITION_INDEPENDENT_CODE=ON -DFAAD_BUILD_CLI=OFF -DFAAD_APPLY_DRC=OFF -DCMAKE_INSTALL_PREFIX=/opt/static \
    && cmake --build /tmp/faad -j"$(nproc)" && cmake --install /tmp/faad
# RTL-SDR library 2.x (the RTL-SDR Blog V4 needs it; distributions often have
# 0.6); it lets go of the DVB-T kernel driver by itself
ARG RTLSDR_VERSION=2.0.3
RUN curl -fsSL https://github.com/osmocom/rtl-sdr/archive/refs/tags/v${RTLSDR_VERSION}.tar.gz | tar xz -C /tmp \
    && cmake -S /tmp/rtl-sdr-${RTLSDR_VERSION} -B /tmp/rtlsdr -DCMAKE_BUILD_TYPE=Release -DDETACH_KERNEL_DRIVER=ON \
       -DINSTALL_UDEV_RULES=OFF -DCMAKE_INSTALL_PREFIX=/opt/rtlsdr \
    && cmake --build /tmp/rtlsdr -j"$(nproc)" && cmake --install /tmp/rtlsdr
ENV FAAD_LIB_DIR=/opt/static/lib OPUS_LIB_DIR=/opt/static/lib

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
# the RTL-SDR library next to the program comes before the system's
ENV RUSTFLAGS="-C link-arg=-Wl,-rpath,\$ORIGIN -C link-arg=-Wl,--disable-new-dtags"
RUN --mount=type=cache,target=/root/.cargo/registry --mount=type=cache,target=/src/target \
    env -u CARGO_BUILD_JOBS cargo build ${CARGO_BUILD_JOBS:+-j $CARGO_BUILD_JOBS} --release -p magicdab --features embed-web \
    && mkdir -p /out/MagicDAB \
    && cp target/release/magicdab /out/MagicDAB/ \
    && strip /out/MagicDAB/magicdab \
    && cp -L /opt/rtlsdr/lib/librtlsdr.so.0 /out/MagicDAB/ \
    && objdump -p /out/MagicDAB/magicdab | grep -E "NEEDED|RPATH|RUNPATH" > /out/needed.txt \
    && objdump -T /out/MagicDAB/magicdab | grep -o "GLIBC_[0-9.]*" | sort -uV | tail -1 >> /out/needed.txt
COPY build/linux/README.txt build/linux/60-rtl-sdr.rules /out/MagicDAB/
COPY LICENSE LICENSE-EXCEPTION.txt THIRD-PARTY.md /out/MagicDAB/

FROM scratch
COPY --from=build /out /
