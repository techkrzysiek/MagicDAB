#!/bin/sh
# Android build in Docker: dist/android/MagicDAB-<version>-android-debug.apk,
# a debug build that installs on any phone (APK=release: the release build,
# unsigned). The image holds the NDK, the SDK and Gradle: about 8 GB of disk
# with its caches. CI builds the same without Docker
# (.github/workflows/android.yml). See docs/android.md.
#
#   build/android.sh
#   ABIS="arm64-v8a x86_64" APK="debug release" JOBS=1 build/android.sh
set -e
cd "$(dirname "$0")/.."
VERSION="${VERSION:-0.3.$(git rev-list --count HEAD)-dev}"
VERSION_CODE="${VERSION_CODE:-$(git rev-list --count HEAD)}"
OUT=dist/android
# JOBS=1: leave a core to a receiver running on the same host
docker build -f build/android/Dockerfile --build-arg VERSION="$VERSION" --build-arg VERSION_CODE="$VERSION_CODE" \
    --build-arg SOURCE_URL="${SOURCE_URL-}" --build-arg UPDATE_URL="${UPDATE_URL-}" --build-arg ABIS="${ABIS:-arm64-v8a}" --build-arg APK="${APK:-debug}" \
    --build-arg JOBS="${JOBS:-}" -o "$OUT" .
ls -l "$OUT"
