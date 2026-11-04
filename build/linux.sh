#!/bin/sh
# Linux build for the architecture of this host (x64 or arm64):
# dist/linux/MagicDAB-<version>-linux-<arch>.zip and the update manifest
# dist/linux/latest-linux-<arch>.json (merged into latest.json by a release).
#
# Same variables as build/windows.sh: VERSION, UPDATE_URL, SOURCE_URL,
# PACKAGE_URL, JOBS (JOBS=1 next to a running receiver).
set -e
cd "$(dirname "$0")/.."
# (the commits less one: the third one was released as 0.3.2)
VERSION="${VERSION:-0.3.$(($(git rev-list --count HEAD) - 1))}"
case "$(uname -m)" in
    x86_64) ARCH=x64 ;;
    aarch64|arm64) ARCH=arm64 ;;
    *) echo "unsupported architecture $(uname -m)"; exit 1 ;;
esac
OUT=dist/linux
rm -rf "$OUT/build"
docker build -f build/linux/Dockerfile --build-arg VERSION="$VERSION" --build-arg UPDATE_URL="${UPDATE_URL-}" \
    --build-arg SOURCE_URL="${SOURCE_URL-}" --build-arg CARGO_BUILD_JOBS="${JOBS:-}" -o "$OUT/build" .
cat "$OUT/build/needed.txt"
python3 - "$OUT" "$VERSION" "$ARCH" "${PACKAGE_URL:-}" <<'PY'
import hashlib, json, os, stat, sys, zipfile
out, version, arch, package_url = sys.argv[1:5]
name = f"MagicDAB-{version}-linux-{arch}.zip"
path = os.path.join(out, name)
root = os.path.join(out, "build", "MagicDAB")
with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
    for f in sorted(os.listdir(root)):
        info = zipfile.ZipInfo(f"MagicDAB/{f}")
        info.compress_type = zipfile.ZIP_DEFLATED
        # unzip keeps the modes: the program and its library stay executable
        executable = f == "magicdab" or ".so" in f
        info.external_attr = ((0o755 if executable else 0o644) | stat.S_IFREG) << 16
        z.writestr(info, open(os.path.join(root, f), "rb").read())
sha = hashlib.sha256(open(path, "rb").read()).hexdigest()
url = package_url.rstrip("/") + "/" + name if package_url else name
json.dump({"version": version, f"linux-{arch}": {"url": url, "sha256": sha}}, open(os.path.join(out, f"latest-linux-{arch}.json"), "w"), indent=2)
print(name, sha)
PY
