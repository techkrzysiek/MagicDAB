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
# Installed copies take a manifest only with its signature (latest.json.sig,
# crates/server/src/update.rs): SIGNING_KEY=FILE (the Ed25519 key, PEM) signs
# it here; without it the manifest is for a release, which signs it itself.
#
# Release builds (.github/workflows/release.yml) also set SOURCE_URL (shown in
# About), PACKAGE_URL (the folder the package is published in; without it the
# manifest points next to itself) and NOTES (shown with the update).
set -e
cd "$(dirname "$0")/.."
# (the next release: the highest v0.3.* tag plus one; git describe takes
# either of two tags on one commit, an older one too)
last=$(git tag --list 'v0.3.*' | sed -n 's/^v0\.3\.\([0-9][0-9]*\)$/\1/p' | sort -n | tail -n 1)
VERSION="${VERSION:-0.3.$((${last:-0} + 1))}"
# e.g. https://github.com/OWNER/REPO/releases/latest/download/latest.json
UPDATE_URL="${UPDATE_URL-}"
SOURCE_URL="${SOURCE_URL-}"
OUT=dist/windows
rm -rf "$OUT/build"
# JOBS=1: leave a core to a receiver running on the same host
docker build -f build/windows/Dockerfile --build-arg VERSION="$VERSION" --build-arg UPDATE_URL="$UPDATE_URL" --build-arg SOURCE_URL="$SOURCE_URL" \
    --build-arg CARGO_BUILD_JOBS="${JOBS:-}" ${FEATURES:+--build-arg FEATURES="$FEATURES"} -o "$OUT/build" .
python3 - "$OUT" "$VERSION" "${PUBLISH:-}" "${PACKAGE_URL:-}" "${NOTES:-}" "${SIGNING_KEY:-}" <<'PY'
import glob, hashlib, json, os, shutil, subprocess, sys, zipfile
out, version, publish, package_url, notes, signing_key = sys.argv[1:7]
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
sig = os.path.join(out, "latest.json.sig")
if os.path.exists(sig):
    os.remove(sig)
if signing_key:
    subprocess.run(["openssl", "pkeyutl", "-sign", "-inkey", signing_key, "-rawin", "-in", os.path.join(out, "latest.json"), "-out", sig], check=True)
if publish:
    os.makedirs(publish, exist_ok=True)
    shutil.copy(path, publish)
    # the manifest last: clients never see a package that is not there yet
    # (its signature just before it)
    for f in ["latest.json.sig", "latest.json"] if os.path.exists(sig) else ["latest.json"]:
        tmp = os.path.join(publish, f + ".tmp")
        shutil.copy(os.path.join(out, f), tmp)
        os.replace(tmp, os.path.join(publish, f))
    old = sorted(glob.glob(os.path.join(publish, "MagicDAB-*-windows-x64.zip")), key=os.path.getmtime)
    for f in old[:-2]:
        os.remove(f)
    print("published in", publish)
PY
