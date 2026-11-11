# syntax=docker/dockerfile:1
# MagicDAB - web based DAB/DAB+ receiver

# ---- web UI (only needed at build time) ----
FROM node:20-trixie-slim AS web
WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY web/ ./
# (without the source maps of web/vite.config.ts: nothing in the package loads them)
RUN npm run build && find dist -name '*.map' -delete

# ---- server ----
FROM rust:1-slim-trixie AS build
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libopus-dev cmake make gcc libc6-dev curl ca-certificates \
    && rm -rf /var/lib/apt/lists/*
# faad2 of our own, static, without the dynamic range control: the gains it
# makes of some stations' DRC data (one station: +25 dB) distort them. Debian's
# libfaad2 applies it.
ARG FAAD_VERSION=2.11.2
RUN curl -fsSL --retry 5 --retry-all-errors https://github.com/knik0/faad2/archive/refs/tags/${FAAD_VERSION}.tar.gz | tar xz -C /tmp \
    && cmake -S /tmp/faad2-${FAAD_VERSION} -B /tmp/faad -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=OFF \
       -DCMAKE_POSITION_INDEPENDENT_CODE=ON -DFAAD_BUILD_CLI=OFF -DFAAD_APPLY_DRC=OFF -DCMAKE_INSTALL_PREFIX=/opt/static \
    && cmake --build /tmp/faad -j1 && cmake --install /tmp/faad && rm -rf /tmp/faad /tmp/faad2-${FAAD_VERSION}
ENV FAAD_LIB_DIR=/opt/static/lib
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY third_party ./third_party
# fewer parallel jobs next to a running receiver: docker compose build --build-arg CARGO_BUILD_JOBS=1
# (only as -j: cargo itself refuses the variable when it is empty)
ARG CARGO_BUILD_JOBS=
# release builds: the version and the address of the source code (AGPL section 13)
ARG VERSION=
ARG SOURCE_URL=
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    env ${VERSION:+MAGICDAB_VERSION=$VERSION} MAGICDAB_SOURCE_URL="$SOURCE_URL" \
    env -u CARGO_BUILD_JOBS cargo build ${CARGO_BUILD_JOBS:+-j $CARGO_BUILD_JOBS} --release -p magicdab && cp target/release/magicdab /usr/local/bin/magicdab \
    && strip --strip-debug /usr/local/bin/magicdab

# ---- runtime ----
FROM debian:trixie-slim
RUN apt-get update && apt-get install -y --no-install-recommends librtlsdr0 libhackrf0 libusb-1.0-0 libopus0 libasound2t64 \
    ca-certificates tzdata && rm -rf /var/lib/apt/lists/*
COPY --from=build /usr/local/bin/magicdab /usr/local/bin/magicdab
COPY --from=web /web/dist /usr/share/magicdab/web
COPY LICENSE LICENSE-EXCEPTION.txt THIRD-PARTY.md /usr/share/doc/magicdab/
# frp client for the public tunnel (Settings > Tunnel)
COPY --from=fatedier/frpc:v0.61.1 /usr/bin/frpc /usr/local/bin/frpc
ENV MAGICDAB_LISTEN=0.0.0.0:8080 \
    MAGICDAB_DATA_DIR=/data \
    MAGICDAB_WEB_ROOT=/usr/share/magicdab/web
VOLUME /data
EXPOSE 8080
STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/local/bin/magicdab"]
