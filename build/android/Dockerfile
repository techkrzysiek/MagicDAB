# syntax=docker/dockerfile:1
# MagicDAB for Android (the APK) with the NDK, the SDK and Gradle in the image,
# for computers without them. Use build/android.sh; the steps themselves are
# build/android/build.sh, which CI runs with the runner's SDK.

# ---- web interface (built into the server) ----
FROM node:20-trixie-slim AS web
WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY web/ ./
RUN npm run build

# ---- NDK, SDK, Gradle, Rust for phones ----
FROM rust:1-slim-trixie AS toolchain
RUN apt-get update && apt-get install -y --no-install-recommends cmake make curl ca-certificates unzip openjdk-21-jdk-headless bzip2 patch \
    && rm -rf /var/lib/apt/lists/*
RUN rustup target add aarch64-linux-android x86_64-linux-android armv7-linux-androideabi
# the NDK: C compilers and the Android C library (the codecs and the server)
ARG NDK_VERSION=r27c
RUN curl -fsSL -o /tmp/ndk.zip https://dl.google.com/android/repository/android-ndk-${NDK_VERSION}-linux.zip \
    && unzip -q /tmp/ndk.zip -d /opt && rm /tmp/ndk.zip && mv /opt/android-ndk-${NDK_VERSION} /opt/ndk
# the SDK: the platform the app is compiled against and the build tools of the Android Gradle plugin
ARG CMDLINE_TOOLS=11076708
ENV ANDROID_HOME=/opt/sdk ANDROID_NDK_HOME=/opt/ndk
RUN curl -fsSL -o /tmp/tools.zip https://dl.google.com/android/repository/commandlinetools-linux-${CMDLINE_TOOLS}_latest.zip \
    && mkdir -p $ANDROID_HOME/cmdline-tools && unzip -q /tmp/tools.zip -d $ANDROID_HOME/cmdline-tools \
    && mv $ANDROID_HOME/cmdline-tools/cmdline-tools $ANDROID_HOME/cmdline-tools/latest && rm /tmp/tools.zip \
    && yes | $ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager --licenses > /dev/null \
    && $ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager "platforms;android-36" "build-tools;35.0.0"
# (the version of android/gradle/wrapper/gradle-wrapper.properties)
ARG GRADLE_VERSION=8.14.3
RUN curl -fsSL -o /tmp/gradle.zip https://services.gradle.org/distributions/gradle-${GRADLE_VERSION}-bin.zip \
    && unzip -q /tmp/gradle.zip -d /opt && rm /tmp/gradle.zip && ln -s /opt/gradle-${GRADLE_VERSION}/bin/gradle /usr/local/bin/gradle

# ---- server and app ----
FROM toolchain AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY third_party ./third_party
COPY android ./android
COPY build/android ./build/android
COPY --from=web /web/dist ./web/dist
ARG VERSION=0.2.0
ARG VERSION_CODE=1
# the address of the source code (AGPL section 13)
ARG SOURCE_URL=
ARG ABIS=arm64-v8a
ARG APK=debug
# fewer parallel jobs on a busy host (e.g. next to a running receiver)
ARG JOBS=
# (the cached ~/.android keeps the debug signing key: a new debug APK installs over the last one)
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,target=/src/target \
    --mount=type=cache,target=/root/.gradle --mount=type=cache,target=/root/.android \
    SKIP_WEB=1 VERSION="$VERSION" VERSION_CODE="$VERSION_CODE" SOURCE_URL="$SOURCE_URL" ABIS="$ABIS" APK="$APK" JOBS="$JOBS" \
    build/android/build.sh && mkdir -p /out && cp dist/android/*.apk /out/

FROM scratch
COPY --from=build /out /
