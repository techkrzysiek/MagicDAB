#!/bin/bash
# MagicDAB for Android (docs/android.md): the server built with the NDK for
# phones (android/app/src/main/jniLibs/<ABI>/libmagicdab.so), then the APK with
# Gradle into dist/android. The one recipe of build/android/Dockerfile (via
# build/android.sh) and of CI (.github/workflows/android.yml).
#
#   ANDROID_NDK_HOME=/path/to/ndk ANDROID_HOME=/path/to/sdk build/android/build.sh
#
# ABIS        the phones: arm64-v8a (default); also x86_64 (emulators), armeabi-v7a
# APK         debug (default: installs on any phone), release (signed when
#             MAGICDAB_KEYSTORE etc. are set, see android/app/build.gradle.kts,
#             else unsigned), "debug release", or empty: only the server
# VERSION     0.3.<commits>-dev (a release: its version); VERSION_CODE <commits>
# SOURCE_URL  the address of the source code (About; AGPL section 13)
# JOBS        parallel jobs (1 next to a running receiver)
# SKIP_WEB=1  web/dist is built already
# GRADLE      the Gradle to run (default: android/gradlew if there, else gradle)
# CODECS_DIR  where opus and faad2 are built (default target/android)
set -euo pipefail
cd "$(dirname "$0")/../.."
ROOT=$PWD

NDK="${ANDROID_NDK_HOME:-${ANDROID_NDK_ROOT:-${ANDROID_NDK:-}}}"
if [ -z "$NDK" ] && [ -d "${ANDROID_HOME:-/nonexistent}/ndk" ]; then
    NDK="$ANDROID_HOME/ndk/$(ls "$ANDROID_HOME/ndk" | sort -V | tail -n 1)"
fi
if [ ! -f "$NDK/build/cmake/android.toolchain.cmake" ]; then
    echo "the Android NDK (r27 or newer) is needed: ANDROID_NDK_HOME" >&2
    exit 1
fi
echo "NDK $NDK: $(grep -m 1 Pkg.Revision "$NDK/source.properties" | cut -d = -f 2 | tr -d ' ')"

# minSdk of the app (android/app/build.gradle.kts)
API=26
ABIS="${ABIS:-arm64-v8a}"
APK="${APK-debug}"
VERSION="${VERSION:-0.3.$(git rev-list --count HEAD)-dev}"
VERSION_CODE="${VERSION_CODE:-$(git rev-list --count HEAD)}"
JOBS="${JOBS:-$(nproc)}"
# the codecs of the Windows package (build/windows/Dockerfile)
OPUS_VERSION=1.5.2
FAAD_VERSION=2.11.2
# the stick straight from the app (no driver app): libusb on the file
# descriptor the app gets from Android, librtlsdr with rtlsdr_open_fd
# (build/android/rtlsdr-open-fd.patch); 2.0.3 knows the RTL-SDR Blog V4
LIBUSB_VERSION=1.0.27
RTLSDR_VERSION=2.0.3
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
# sources and builds of the codecs (CI caches it)
DEPS="${CODECS_DIR:-$TARGET_DIR/android}"
case "$(uname -s)" in
    Darwin) TC="$NDK/toolchains/llvm/prebuilt/darwin-x86_64/bin" ;;
    *) TC="$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin" ;;
esac

# the web interface, built into the server (feature embed-web)
if [ -z "${SKIP_WEB:-}" ]; then
    (cd web && npm ci --no-audit --no-fund && npm run build)
fi
if [ ! -f web/dist/index.html ]; then
    echo "web/dist is missing (npm run build in web/)" >&2
    exit 1
fi

target_of() {
    case $1 in
        arm64-v8a) echo aarch64-linux-android ;;
        armeabi-v7a) echo armv7-linux-androideabi ;;
        x86_64) echo x86_64-linux-android ;;
        x86) echo i686-linux-android ;;
        *) echo "unknown ABI $1" >&2; return 1 ;;
    esac
}

# the NDK's compiler of an ABI, for the API level of the app
clang_of() {
    case $1 in
        arm64-v8a) echo "aarch64-linux-android$API-clang" ;;
        armeabi-v7a) echo "armv7a-linux-androideabi$API-clang" ;;
        x86_64) echo "x86_64-linux-android$API-clang" ;;
        x86) echo "i686-linux-android$API-clang" ;;
    esac
}

# opus and faad2 for one ABI as static libraries in $PREFIX/lib (kept between builds)
codecs() {
    local abi=$1 src="$DEPS/src"
    PREFIX="$DEPS/$abi-opus$OPUS_VERSION-faad$FAAD_VERSION"
    if [ -f "$PREFIX/lib/libopus.a" ] && [ -f "$PREFIX/lib/libfaad.a" ]; then
        return
    fi
    mkdir -p "$src"
    [ -d "$src/opus-$OPUS_VERSION" ] || curl -fsSL "https://downloads.xiph.org/releases/opus/opus-$OPUS_VERSION.tar.gz" | tar xz -C "$src"
    [ -d "$src/faad2-$FAAD_VERSION" ] || curl -fsSL "https://github.com/knik0/faad2/archive/refs/tags/$FAAD_VERSION.tar.gz" | tar xz -C "$src"
    # (CMAKE_POLICY_VERSION_MINIMUM: CMake 4 refuses projects written for old versions)
    local android=(
        -DCMAKE_TOOLCHAIN_FILE="$NDK/build/cmake/android.toolchain.cmake"
        -DANDROID_ABI="$abi" -DANDROID_PLATFORM="android-$API"
        -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=OFF
        -DCMAKE_INSTALL_PREFIX="$PREFIX" -DCMAKE_INSTALL_LIBDIR=lib
        -DCMAKE_POLICY_VERSION_MINIMUM=3.5
    )
    cmake -S "$src/opus-$OPUS_VERSION" -B "$DEPS/build/opus-$abi" "${android[@]}" -DOPUS_BUILD_TESTING=OFF -DOPUS_BUILD_PROGRAMS=OFF
    cmake --build "$DEPS/build/opus-$abi" -j "$JOBS"
    cmake --install "$DEPS/build/opus-$abi"
    cmake -S "$src/faad2-$FAAD_VERSION" -B "$DEPS/build/faad-$abi" "${android[@]}" -DFAAD_BUILD_CLI=OFF -DFAAD_APPLY_DRC=OFF
    cmake --build "$DEPS/build/faad-$abi" -j "$JOBS"
    cmake --install "$DEPS/build/faad-$abi"
}

# librtlsdr.so for one ABI (libusb linked in), into $RTLSDR_SO
rtlsdr() {
    local abi=$1 src="$DEPS/src" cc=$2
    local usb="$DEPS/$abi-libusb$LIBUSB_VERSION"
    RTLSDR_SO="$DEPS/$abi-rtlsdr$RTLSDR_VERSION/librtlsdr.so"
    if [ -f "$RTLSDR_SO" ]; then
        return
    fi
    mkdir -p "$src"
    [ -d "$src/libusb-$LIBUSB_VERSION" ] || curl -fsSL "https://github.com/libusb/libusb/releases/download/v$LIBUSB_VERSION/libusb-$LIBUSB_VERSION.tar.bz2" | tar xj -C "$src"
    if [ ! -d "$src/rtl-sdr-$RTLSDR_VERSION" ]; then
        curl -fsSL "https://github.com/osmocom/rtl-sdr/archive/refs/tags/v$RTLSDR_VERSION.tar.gz" | tar xz -C "$src"
        patch -p1 -d "$src/rtl-sdr-$RTLSDR_VERSION" < build/android/rtlsdr-open-fd.patch
    fi
    if [ ! -f "$usb/lib/libusb-1.0.a" ]; then
        # (out of the source tree: a copy has new times and make would run autotools)
        rm -rf "$DEPS/build/libusb-$abi" && mkdir -p "$DEPS/build/libusb-$abi"
        (cd "$DEPS/build/libusb-$abi" && "$src/libusb-$LIBUSB_VERSION/configure" -q --host="$(target_of "$abi")" CC="$cc" AR="$TC/llvm-ar" RANLIB="$TC/llvm-ranlib" \
            --disable-udev --enable-static --disable-shared --with-pic --prefix="$usb" && make -j "$JOBS" >/dev/null && make install >/dev/null)
    fi
    local r="$src/rtl-sdr-$RTLSDR_VERSION"
    mkdir -p "$(dirname "$RTLSDR_SO")"
    "$cc" -shared -fPIC -O2 -I"$r/include" -I"$usb/include/libusb-1.0" \
        "$r/src/librtlsdr.c" "$r/src/tuner_e4k.c" "$r/src/tuner_fc0012.c" "$r/src/tuner_fc0013.c" "$r/src/tuner_fc2580.c" "$r/src/tuner_r82xx.c" \
        "$usb/lib/libusb-1.0.a" -llog -Wl,-soname,librtlsdr.so -Wl,-z,max-page-size=16384 -o "$RTLSDR_SO"
}

JNI=android/app/src/main/jniLibs
rm -rf "$JNI"
for abi in $ABIS; do
    target=$(target_of "$abi")
    codecs "$abi"
    cc="$TC/$(clang_of "$abi")"
    mkdir -p "$DEPS/build"
    rtlsdr "$abi" "$cc"
    upper=$(echo "$target" | tr 'a-z-' 'A-Z_')
    if command -v rustup >/dev/null; then
        rustup target add "$target"
    fi
    echo "== server for $abi ($target)"
    # the server is a program: the APK's lib*.so name only gets it unpacked
    # (useLegacyPackaging); 16 KB pages of newer phones need the alignment
    env "CC_${target//-/_}=$cc" "AR_${target//-/_}=$TC/llvm-ar" \
        "CARGO_TARGET_${upper}_LINKER=$cc" \
        "CARGO_TARGET_${upper}_RUSTFLAGS=-C link-arg=-Wl,-z,max-page-size=16384" \
        FAAD_LIB_DIR="$PREFIX/lib" OPUS_LIB_DIR="$PREFIX/lib" \
        MAGICDAB_VERSION="$VERSION" MAGICDAB_SOURCE_URL="${SOURCE_URL:-}" \
        CARGO_PROFILE_RELEASE_STRIP=symbols \
        cargo build -j "$JOBS" --release --target "$target" -p magicdab --features embed-web
    mkdir -p "$JNI/$abi"
    cp "$TARGET_DIR/$target/release/magicdab" "$JNI/$abi/libmagicdab.so"
    cp "$RTLSDR_SO" "$JNI/$abi/librtlsdr.so"
    ls -l "$JNI/$abi/libmagicdab.so"
done

[ -n "$APK" ] || exit 0
if [ -z "${GRADLE:-}" ]; then
    if [ -x android/gradlew ]; then GRADLE="$ROOT/android/gradlew"; else GRADLE=gradle; fi
fi
tasks=()
for variant in $APK; do
    case $variant in
        debug) tasks+=(assembleDebug) ;;
        release) tasks+=(assembleRelease) ;;
        *) echo "APK: debug and/or release, not $variant" >&2; exit 1 ;;
    esac
done
(cd android && "$GRADLE" --no-daemon "${tasks[@]}" -PmagicdabVersion="$VERSION" -PmagicdabVersionCode="$VERSION_CODE")
mkdir -p dist/android
for variant in $APK; do
    for apk in android/app/build/outputs/apk/"$variant"/*.apk; do
        case $apk in
            *-unsigned.apk) name="MagicDAB-$VERSION-android-unsigned.apk" ;;
            *-debug.apk) name="MagicDAB-$VERSION-android-debug.apk" ;;
            *) name="MagicDAB-$VERSION-android.apk" ;;
        esac
        cp "$apk" "dist/android/$name"
        echo "dist/android/$name"
    done
done
