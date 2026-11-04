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
RUN apt-get update && apt-get install -y --no-install-recommends mingw-w64 cmake make curl ca-certificates unzip \
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

# RTL-SDR library (rtl-sdr-blog drivers; the dongle needs WinUSB, e.g. via Zadig)
ARG RTLSDR_RELEASE=V1.4.0
RUN curl -fsSL --retry 5 --retry-all-errors -o /tmp/rtlsdr.zip https://github.com/rtlsdrblog/rtl-sdr-blog/releases/download/${RTLSDR_RELEASE}/Release.zip \
    && mkdir -p /opt/rtlsdr \
    && unzip -j /tmp/rtlsdr.zip x64/rtlsdr.dll x64/msvcr100.dll x64/pthreadVC2.dll x64/rtl_test.exe -d /opt/rtlsdr
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
    && x86_64-w64-mingw32-objdump -p /out/MagicDAB/magicdab.exe | grep "DLL Name" > /out/dlls.txt
COPY build/windows/README.txt /out/MagicDAB/README.txt
COPY LICENSE LICENSE-EXCEPTION.txt THIRD-PARTY.md /out/MagicDAB/

FROM scratch
COPY --from=build /out /
