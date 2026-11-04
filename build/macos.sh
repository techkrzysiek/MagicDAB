#!/bin/sh
# macOS build, on a Mac (the macos-14 runner of GitHub): one universal
# program (Apple silicon and Intel) with the web interface inside and the
# codecs linked in, the RTL-SDR library and libusb next to it:
# dist/macos/MagicDAB-<version>-macos.zip and dist/macos/latest-macos.json.
#
# Needs the Xcode command line tools, cmake, node and rustup with the targets
# aarch64-apple-darwin and x86_64-apple-darwin. Same variables as
# build/windows.sh: VERSION, UPDATE_URL, SOURCE_URL, PACKAGE_URL.
set -e
cd "$(dirname "$0")/.."
# (the next release: the last v0.3.* tag plus one)
last_tag=$(git describe --tags --abbrev=0 --match 'v0.3.*' 2>/dev/null || echo v0.3.0)
VERSION="${VERSION:-0.3.$((${last_tag##*.} + 1))}"
OUT=dist/macos
DEPS="$PWD/target/macos-deps"
OPUS_VERSION=1.5.2
FAAD_VERSION=2.11.2
LIBUSB_VERSION=1.0.27
RTLSDR_VERSION=2.0.3
export MACOSX_DEPLOYMENT_TARGET=11.0
rm -rf "$OUT/build"
mkdir -p "$OUT/build/MagicDAB" "$DEPS/src"

(cd web && npm ci --no-audit --no-fund && npm run build)

fetch() {
    [ -d "$DEPS/src/$2" ] || curl -fsSL --retry 5 --retry-all-errors "$1" | tar x -C "$DEPS/src" -f -
}
fetch "https://github.com/xiph/opus/releases/download/v$OPUS_VERSION/opus-$OPUS_VERSION.tar.gz" "opus-$OPUS_VERSION"
fetch "https://github.com/knik0/faad2/archive/refs/tags/$FAAD_VERSION.tar.gz" "faad2-$FAAD_VERSION"
fetch "https://github.com/libusb/libusb/releases/download/v$LIBUSB_VERSION/libusb-$LIBUSB_VERSION.tar.bz2" "libusb-$LIBUSB_VERSION"
fetch "https://github.com/osmocom/rtl-sdr/archive/refs/tags/v$RTLSDR_VERSION.tar.gz" "rtl-sdr-$RTLSDR_VERSION"

# every architecture on its own (the codecs choose their SIMD code per
# architecture), joined with lipo at the end
for arch in arm64 x86_64; do
    p="$DEPS/$arch"
    [ "$arch" = arm64 ] && target=aarch64-apple-darwin || target=x86_64-apple-darwin
    if [ ! -f "$p/lib/libopus.a" ]; then
        cmake -S "$DEPS/src/opus-$OPUS_VERSION" -B "$DEPS/build/opus-$arch" -DCMAKE_OSX_ARCHITECTURES=$arch -DCMAKE_BUILD_TYPE=Release \
            -DBUILD_SHARED_LIBS=OFF -DOPUS_BUILD_TESTING=OFF -DOPUS_BUILD_PROGRAMS=OFF -DCMAKE_INSTALL_PREFIX="$p"
        cmake --build "$DEPS/build/opus-$arch" -j4 && cmake --install "$DEPS/build/opus-$arch"
    fi
    if [ ! -f "$p/lib/libfaad.a" ]; then
        cmake -S "$DEPS/src/faad2-$FAAD_VERSION" -B "$DEPS/build/faad-$arch" -DCMAKE_OSX_ARCHITECTURES=$arch -DCMAKE_BUILD_TYPE=Release \
            -DBUILD_SHARED_LIBS=OFF -DFAAD_BUILD_CLI=OFF -DCMAKE_INSTALL_PREFIX="$p"
        cmake --build "$DEPS/build/faad-$arch" -j4 && cmake --install "$DEPS/build/faad-$arch"
    fi
    if [ ! -f "$p/lib/libusb-1.0.0.dylib" ]; then
        rm -rf "$DEPS/build/libusb-$arch" && cp -R "$DEPS/src/libusb-$LIBUSB_VERSION" "$DEPS/build/libusb-$arch"
        [ "$arch" = arm64 ] && host=aarch64-apple-darwin || host=x86_64-apple-darwin
        (cd "$DEPS/build/libusb-$arch" && ./configure --host=$host --prefix="$p" --disable-static CC="clang -arch $arch" \
            && make -j4 && make install)
    fi
    if [ ! -f "$p/lib/librtlsdr.0.dylib" ]; then
        PKG_CONFIG_PATH="$p/lib/pkgconfig" cmake -S "$DEPS/src/rtl-sdr-$RTLSDR_VERSION" -B "$DEPS/build/rtlsdr-$arch" \
            -DCMAKE_OSX_ARCHITECTURES=$arch -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$p" -DCMAKE_PREFIX_PATH="$p"
        cmake --build "$DEPS/build/rtlsdr-$arch" -j4 && cmake --install "$DEPS/build/rtlsdr-$arch"
    fi
    # the libraries find each other next to the program (per architecture:
    # each refers to its own libusb)
    s="$DEPS/stage-$arch"
    rm -rf "$s" && mkdir -p "$s"
    cp -L "$p/lib/librtlsdr.0.dylib" "$p/lib/libusb-1.0.0.dylib" "$s/"
    install_name_tool -id @loader_path/libusb-1.0.0.dylib "$s/libusb-1.0.0.dylib"
    install_name_tool -id @loader_path/librtlsdr.0.dylib "$s/librtlsdr.0.dylib"
    for old in $(otool -L "$s/librtlsdr.0.dylib" | awk '/libusb-1.0/ {print $1}' | sort -u); do
        install_name_tool -change "$old" @loader_path/libusb-1.0.0.dylib "$s/librtlsdr.0.dylib"
    done
    # (pkg-config would find codecs of Homebrew: the static ones here instead)
    PKG_CONFIG_LIBDIR=/nonexistent FAAD_LIB_DIR="$p/lib" OPUS_LIB_DIR="$p/lib" \
        MAGICDAB_VERSION="$VERSION" MAGICDAB_UPDATE_URL="${UPDATE_URL-}" MAGICDAB_SOURCE_URL="${SOURCE_URL-}" \
        cargo build --release --target $target -p magicdab --features embed-web
done

b="$OUT/build/MagicDAB"
lipo -create target/aarch64-apple-darwin/release/magicdab target/x86_64-apple-darwin/release/magicdab -output "$b/magicdab"
lipo -create "$DEPS/stage-arm64/librtlsdr.0.dylib" "$DEPS/stage-x86_64/librtlsdr.0.dylib" -output "$b/librtlsdr.0.dylib"
lipo -create "$DEPS/stage-arm64/libusb-1.0.0.dylib" "$DEPS/stage-x86_64/libusb-1.0.0.dylib" -output "$b/libusb-1.0.0.dylib"
# (changed files lose their signature; Apple silicon runs nothing unsigned)
for f in "$b/magicdab" "$b/librtlsdr.0.dylib" "$b/libusb-1.0.0.dylib"; do
    codesign --force --sign - "$f"
done
otool -L "$b/magicdab" "$b/librtlsdr.0.dylib"
lipo -info "$b/magicdab"
cp build/macos/README.txt LICENSE LICENSE-EXCEPTION.txt THIRD-PARTY.md "$b/"

name="MagicDAB-$VERSION-macos.zip"
# (zip keeps the modes and the signatures)
(cd "$OUT/build" && rm -f "../$name" && zip -qry "../$name" MagicDAB)
sha=$(shasum -a 256 "$OUT/$name" | cut -d' ' -f1)
url="$name"
[ -n "${PACKAGE_URL:-}" ] && url="${PACKAGE_URL%/}/$name"
# one universal package for both kinds of Mac
cat > "$OUT/latest-macos.json" <<JSON
{
  "version": "$VERSION",
  "macos-arm64": {"url": "$url", "sha256": "$sha"},
  "macos-x64": {"url": "$url", "sha256": "$sha"}
}
JSON
echo "$name $sha"
