#!/bin/sh
# Windows build: dist/windows/MagicDAB-<version>-windows-x64.zip and the update
# manifest dist/windows/latest.json next to it.
#
# Installed copies check UPDATE_URL (built in as the default of Settings >
# Internet > Updates) every 12 hours and install a newer package with one click.
# PUBLISH=DIR copies the package and the manifest there, e.g. the data/updates
# folder of a running MagicDAB, which serves it at /updates/:
#
#   PUBLISH=data/updates build/windows.sh
#
# Release builds (.github/workflows/release.yml) also set SOURCE_URL (shown in
# About), PACKAGE_URL (the folder the package is published in; without it the
# manifest points next to itself) and NOTES (shown with the update).
set -e
cd "$(dirname "$0")/.."
# (the commits less one: the third one was released as 0.3.2)
VERSION="${VERSION:-0.3.$(($(git rev-list --count HEAD) - 1))}"
# e.g. https://github.com/OWNER/REPO/releases/latest/download/latest.json
UPDATE_URL="${UPDATE_URL-}"
SOURCE_URL="${SOURCE_URL-}"
OUT=dist/windows
rm -rf "$OUT/build"
# JOBS=1: leave a core to a receiver running on the same host
docker build -f build/windows/Dockerfile --build-arg VERSION="$VERSION" --build-arg UPDATE_URL="$UPDATE_URL" --build-arg SOURCE_URL="$SOURCE_URL" \
    --build-arg CARGO_BUILD_JOBS="${JOBS:-}" ${FEATURES:+--build-arg FEATURES="$FEATURES"} -o "$OUT/build" .
python3 - "$OUT" "$VERSION" "${PUBLISH:-}" "${PACKAGE_URL:-}" "${NOTES:-}" <<'PY'
import glob, hashlib, json, os, shutil, sys, zipfile
out, version, publish, package_url, notes = sys.argv[1:6]
name = f"MagicDAB-{version}-windows-x64.zip"
path = os.path.join(out, name)
with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
    root = os.path.join(out, "build", "MagicDAB")
    for f in sorted(os.listdir(root)):
        z.write(os.path.join(root, f), f"MagicDAB/{f}")
sha = hashlib.sha256(open(path, "rb").read()).hexdigest()
# the package address is relative to the manifest unless its folder is given
url = package_url.rstrip("/") + "/" + name if package_url else name
manifest = {"version": version, "windows-x64": {"url": url, "sha256": sha}}
if notes:
    manifest["notes"] = notes
json.dump(manifest, open(os.path.join(out, "latest.json"), "w"), indent=2)
print(name, sha)
if publish:
    os.makedirs(publish, exist_ok=True)
    shutil.copy(path, publish)
    # the manifest last: clients never see a package that is not there yet
    tmp = os.path.join(publish, "latest.json.tmp")
    shutil.copy(os.path.join(out, "latest.json"), tmp)
    os.replace(tmp, os.path.join(publish, "latest.json"))
    old = sorted(glob.glob(os.path.join(publish, "MagicDAB-*-windows-x64.zip")), key=os.path.getmtime)
    for f in old[:-2]:
        os.remove(f)
    print("published in", publish)
PY
