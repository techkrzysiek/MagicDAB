#!/bin/sh
# Linux arm64 (Raspberry Pi 3/4/5 with a 64-bit OS) cross-compiled on an
# x86_64 host: dist/linux-arm64/magicdab with the web interface inside.
# Same variables as build/linux.sh: VERSION, UPDATE_URL, SOURCE_URL, JOBS
# (JOBS=1 next to a running receiver).
set -e
cd "$(dirname "$0")/.."
# (the next release: the highest v0.3.* tag plus one; git describe takes
# either of two tags on one commit, an older one too)
last=$(git tag --list 'v0.3.*' | sed -n 's/^v0\.3\.\([0-9][0-9]*\)$/\1/p' | sort -n | tail -n 1)
VERSION="${VERSION:-0.3.$((${last:-0} + 1))}"
OUT=dist/linux-arm64
rm -rf "$OUT"
docker build -f build/linux-arm64/Dockerfile --build-arg VERSION="$VERSION" --build-arg UPDATE_URL="${UPDATE_URL-}" \
    --build-arg SOURCE_URL="${SOURCE_URL-}" --build-arg CARGO_BUILD_JOBS="${JOBS:-}" -o "$OUT" .
cat "$OUT/needed.txt"
ls -l "$OUT/magicdab"
