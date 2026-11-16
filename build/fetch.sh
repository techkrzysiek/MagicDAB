#!/bin/sh
# A source or a tool for a build: downloaded, checked against its SHA-256 in
# sources.sha256 (next to this script) and written to standard output.
# Nothing unchecked reaches a build: an archive that changed where it is kept,
# or a version whose checksum is not in the list yet, ends it.
#
#   build/fetch.sh URL | tar xz -C /tmp
set -eu
url=$1
list=$(dirname "$0")/sources.sha256
sum=$(awk -v u="$url" '$2 == u { print $1 }' "$list")
if [ -z "$sum" ]; then
    echo "fetch.sh: no checksum for $url in $list" >&2
    exit 1
fi
tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT
curl -fsSL --retry 5 --retry-all-errors -o "$tmp" "$url"
if command -v sha256sum > /dev/null 2>&1; then
    got=$(sha256sum "$tmp" | cut -d' ' -f1)
else
    got=$(shasum -a 256 "$tmp" | cut -d' ' -f1)
fi
if [ "$got" != "$sum" ]; then
    echo "fetch.sh: $url has the checksum $got, not $sum" >&2
    exit 1
fi
cat "$tmp"
