#!/usr/bin/env bash
# Install the PDFium renderer from the existing checksum-verified asset cache.
# The caller owns fetching and chooses the prefix: the OS image and the CI
# builder both install to /usr.
set -euo pipefail

version=8057 # Same upstream Chromium release as tools/windows/install-pdfium.ps1.
case "$(uname -m)" in
  x86_64) architecture=x64 ;;
  aarch64) architecture=arm64 ;;
  *) echo 'unsupported native PDFium architecture' >&2; exit 1 ;;
esac
asset="pdfium-${version}-linux-${architecture}.tgz"
if [[ $# == 1 && $1 == --asset-name ]]; then
  printf '%s\n' "$asset"
  exit 0
fi
if [[ $# != 3 ]]; then
  echo 'usage: install-pdfium.sh CACHE ABSOLUTE_PREFIX ASSET_MANIFEST | --asset-name' >&2
  exit 2
fi
cache=$1
destination=$2
manifest=$3
[[ $destination == /* ]] || { echo 'PDFium destination must be absolute' >&2; exit 2; }
expected=$(awk -F '\t' -v name="$asset" '$1 !~ /^#/ && $2 == name {print $1}' "$manifest")
[[ $expected =~ ^[0-9a-f]{64}$ ]] || { echo "missing or ambiguous digest for $asset" >&2; exit 1; }
printf '%s  %s\n' "$expected" "$cache/$asset" | sha256sum -c - >&2

# Extract only the renderer and its upstream licenses/version, never tools or
# headers. Verification and native loading precede destination writes on retry.
stage=$(mktemp -d)
trap 'rm -rf -- "$stage"' EXIT
tar -xzf "$cache/$asset" --no-same-owner --no-same-permissions -C "$stage" \
  lib/libpdfium.so LICENSE VERSION licenses
# A present .so alone is insufficient: require native loading and the PDFium
# version API before a test can mistake the pure-Rust fallback for rendering.
python3 - "$stage/lib/libpdfium.so" <<'PY'
import ctypes
import sys

library = ctypes.CDLL(sys.argv[1])
library.FPDF_InitLibrary.argtypes = []
library.FPDF_InitLibrary.restype = None
library.FPDF_DestroyLibrary.argtypes = []
library.FPDF_DestroyLibrary.restype = None
library.FPDF_GetFileVersion.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_int)]
library.FPDF_GetFileVersion.restype = ctypes.c_int
library.FPDF_InitLibrary()
library.FPDF_DestroyLibrary()
PY
library="$destination/lib64/libpdfium.so"
license_directory="$destination/share/licenses/tilo-pdfium/$version"
install -D -m 0755 "$stage/lib/libpdfium.so" "$library"
mkdir -p "$license_directory"
cp -R "$stage/LICENSE" "$stage/VERSION" "$stage/licenses" "$license_directory/"
printf '%s\n' "$library"
