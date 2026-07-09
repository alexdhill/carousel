#!/usr/bin/env bash
# fetch-chromium.sh
# Download a pinned Chromium revision into resources/chromium/ for app packaging.
# Inputs: none (revision pinned below). Output: resources/chromium populated.
# Errors: nonzero exit on download/unzip failure or unsupported OS.
set -euo pipefail

# Pinned Chromium snapshot revision — bump deliberately, never float.
CHROMIUM_REVISION="1465696"
DEST="resources/chromium"
BASE="https://storage.googleapis.com/chromium-browser-snapshots"

uname_s="$(uname -s)"
case "${uname_s}" in
    Darwin)  plat="Mac";   zip="chrome-mac.zip" ;;
    Linux)   plat="Linux_x64"; zip="chrome-linux.zip" ;;
    MINGW*|MSYS*|CYGWIN*) plat="Win_x64"; zip="chrome-win.zip" ;;
    *) echo "unsupported OS: ${uname_s}" >&2; exit 1 ;;
esac

url="${BASE}/${plat}/${CHROMIUM_REVISION}/${zip}"
mkdir -p "${DEST}"
echo "fetching ${url}"
curl -fSL "${url}" -o "${DEST}/${zip}"
( cd "${DEST}" && unzip -oq "${zip}" && rm -f "${zip}" )
echo "chromium unpacked under ${DEST}"
