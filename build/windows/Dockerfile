# syntax=docker/dockerfile:1
# MagicDAB for Windows (x86_64), cross built with MinGW-w64.
# Use build/windows.sh (zip and update manifest).

# ---- web interface (built into the program) ----
FROM node:20-trixie-slim AS web
WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY web/ ./
# (without the source maps of web/vite.config.ts: nothing in the package loads them)
RUN npm run build && find dist -name '*.map' -delete

# ---- MinGW toolchain, codecs, RTL-SDR library ----
FROM rust:1-slim-trixie AS toolchain
RUN apt-get update && apt-get install -y --no-install-recommends mingw-w64 cmake make curl ca-certificates bzip2 unzip \
    && rm -rf /var/lib/apt/lists/*
RUN rustup target add x86_64-pc-windows-gnu

# codecs as static libraries
ARG OPUS_VERSION=1.5.2
ARG FAAD_VERSION=2.11.2
RUN curl -fsSL --retry 5 --retry-all-errors https://github.com/xiph/opus/releases/download/v${OPUS_VERSION}/opus-${OPUS_VERSION}.tar.gz | tar xz -C /tmp \
    && cmake -S /tmp/opus-${OPUS_VERSION} -B /tmp/opus -DCMAKE_SYSTEM_NAME=Windows -DCMAKE_C_COMPILER=x86_64-w64-mingw32-gcc \
       -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=OFF -DOPUS_BUILD_TESTING=OFF -DOPUS_BUILD_PROGRAMS=OFF \
       -DCMAKE_INSTALL_PREFIX=/opt/win \
    && cmake --build /tmp/opus -j"$(nproc)" && cmake --install /tmp/opus
RUN curl -fsSL --retry 5 --retry-all-errors https://github.com/knik0/faad2/archive/refs/tags/${FAAD_VERSION}.tar.gz | tar xz -C /tmp \
    && cmake -S /tmp/faad2-${FAAD_VERSION} -B /tmp/faad -DCMAKE_SYSTEM_NAME=Windows -DCMAKE_C_COMPILER=x86_64-w64-mingw32-gcc \
       -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=OFF -DFAAD_BUILD_CLI=OFF -DFAAD_APPLY_DRC=OFF -DCMAKE_INSTALL_PREFIX=/opt/win \
    && cmake --build /tmp/faad -j"$(nproc)" && cmake --install /tmp/faad

# RTL-SDR library: osmocom rtl-sdr (V3 and V4, the same as on Android) with
# libusb linked in, built here so that rtlsdr.dll needs only what every
# Windows has (KERNEL32, msvcrt). The rtl-sdr-blog release built with Visual
# C++ needed VCRUNTIME140.dll, missing on a clean Windows: "librtlsdr not found
# (LoadLibraryExW failed)". The dongle needs WinUSB (Zadig, in the package).
ARG LIBUSB_VERSION=1.0.27
ARG RTLSDR_VERSION=2.0.3
RUN curl -fsSL --retry 5 --retry-all-errors https://github.com/libusb/libusb/releases/download/v${LIBUSB_VERSION}/libusb-${LIBUSB_VERSION}.tar.bz2 | tar xj -C /tmp \
    && cd /tmp/libusb-${LIBUSB_VERSION} \
    && ./configure -q --host=x86_64-w64-mingw32 --prefix=/opt/win --disable-shared --enable-static \
    && make -s -j"$(nproc)" >/dev/null 2>&1 && make -s install >/dev/null
RUN curl -fsSL --retry 5 --retry-all-errors https://github.com/osmocom/rtl-sdr/archive/refs/tags/v${RTLSDR_VERSION}.tar.gz | tar xz -C /tmp \
    && r=/tmp/rtl-sdr-${RTLSDR_VERSION} && mkdir -p /opt/rtlsdr \
    && x86_64-w64-mingw32-gcc -shared -O2 -s -Drtlsdr_EXPORTS -I$r/include -I/opt/win/include/libusb-1.0 \
       $r/src/librtlsdr.c $r/src/tuner_e4k.c $r/src/tuner_fc0012.c $r/src/tuner_fc0013.c $r/src/tuner_fc2580.c $r/src/tuner_r82xx.c \
       /opt/win/lib/libusb-1.0.a -static-libgcc -Wl,--exclude-libs,ALL -o /opt/rtlsdr/rtlsdr.dll \
    && x86_64-w64-mingw32-gcc -O2 -s -I$r/include -I$r/src $r/src/rtl_test.c $r/src/convenience/convenience.c $r/src/getopt/getopt.c \
       /opt/rtlsdr/rtlsdr.dll -static -o /opt/rtlsdr/rtl_test.exe \
    && ! x86_64-w64-mingw32-objdump -p /opt/rtlsdr/rtlsdr.dll | grep "DLL Name" | grep -viE "kernel32|msvcrt"
# Zadig (GPL-3.0, Pete Batard, https://zadig.akeo.ie) for the WinUSB driver of
# the dongle; source: https://github.com/pbatard/libwdi/tree/v1.5.1
ARG ZADIG_TAG=v1.5.1 ZADIG_VERSION=2.9
ARG ZADIG_SHA256=4ecaa95df3da3621486a043aef8b3050b8bafe7c901402871e816229ef82039b
RUN mkdir -p /opt/zadig \
    && curl -fsSL --retry 5 --retry-all-errors -o /opt/zadig/zadig.exe https://github.com/pbatard/libwdi/releases/download/${ZADIG_TAG}/zadig-${ZADIG_VERSION}.exe \
    && echo "${ZADIG_SHA256}  /opt/zadig/zadig.exe" | sha256sum -c - \
    && curl -fsSL --retry 5 --retry-all-errors -o /opt/zadig/LICENSE-Zadig.txt https://raw.githubusercontent.com/pbatard/libwdi/${ZADIG_TAG}/COPYING
ENV FAAD_LIB_DIR=/opt/win/lib OPUS_LIB_DIR=/opt/win/lib

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
# desktop: the Windows application (own window, tray icon); without it the
# program runs in a console and opens the browser
ARG FEATURES=embed-web,desktop
ENV MAGICDAB_VERSION=${VERSION} MAGICDAB_UPDATE_URL=${UPDATE_URL} MAGICDAB_SOURCE_URL=${SOURCE_URL}
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,target=/src/target \
    env -u CARGO_BUILD_JOBS cargo build ${CARGO_BUILD_JOBS:+-j $CARGO_BUILD_JOBS} --release --target x86_64-pc-windows-gnu -p magicdab --features "$FEATURES" \
    && mkdir -p /out/MagicDAB \
    && cp target/x86_64-pc-windows-gnu/release/magicdab.exe /out/MagicDAB/ \
    && cp /opt/rtlsdr/* /out/MagicDAB/ \
    && if echo "$FEATURES" | grep -q desktop; then \
         cp "$(ls -d /usr/local/cargo/registry/src/*/webview2-com-sys-*/x64 | tail -1)/WebView2Loader.dll" /out/MagicDAB/; fi \
    && for f in magicdab.exe rtlsdr.dll; do echo "$f"; x86_64-w64-mingw32-objdump -p /out/MagicDAB/$f | grep "DLL Name"; done > /out/dlls.txt
COPY build/windows/README.txt /out/MagicDAB/README.txt
COPY build/windows/zadig.ini /out/MagicDAB/zadig.ini
RUN cp /opt/zadig/* /out/MagicDAB/
COPY LICENSE LICENSE-EXCEPTION.txt THIRD-PARTY.md /out/MagicDAB/

FROM scratch
COPY --from=build /out /
