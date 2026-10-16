#!/usr/bin/env bash
# Package a cross-compiled Windows imager on Linux: the standalone EXE, the
# portable zip and the Inno Setup installer (compiled by Inno Setup under
# Wine), each with a SHA-256 sidecar, the same set package-host.ps1 makes on a
# Windows runner.
#
#   package-windows.sh <x86_64|aarch64> <imager.exe> <out-dir> [sign-command]
#
# sign-command, when given, is run as `sign-command <file>` on the bundled EXE
# before it is archived and on the installer after it is compiled, so the
# installer carries the signed EXE. Without it nothing is signed.
#
# ISCC names Inno Setup's command-line compiler (default
# /opt/inno-setup/ISCC.exe, where the builder image installs it). Wine gets a
# fresh prefix under TMPDIR for every run, so nothing of an earlier run leaks
# into the package.
set -euo pipefail

arch="${1:?architecture: x86_64 or aarch64}"
exe="${2:?the imager.exe to package}"
out="${3:?output directory}"
sign="${4:-}"
repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)"
iscc="${ISCC:-/opt/inno-setup/ISCC.exe}"

version="${TILO_VERSION:-0.1.0}"
version="${version#imager-v}"
version="${version#v}"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
  || { printf 'version must be MAJOR.MINOR.PATCH, not %s\n' "$version" >&2; exit 2; }

case "$arch" in
  x86_64) machine=8664 installer_arch=x64compatible ;;
  aarch64) machine=aa64 installer_arch=arm64 ;;
  *) printf 'unsupported architecture: %s\n' "$arch" >&2; exit 2 ;;
esac

# The PE header's machine field must match the package: a package for one
# architecture must never carry the other's executable.
python3 -I - "$exe" "$machine" <<'PY'
import struct, sys
path, expected = sys.argv[1], int(sys.argv[2], 16)
data = open(path, "rb").read(4096)
if len(data) < 64 or data[:2] != b"MZ":
    sys.exit(f"{path}: not a Windows executable")
offset = struct.unpack_from("<I", data, 0x3C)[0]
if offset + 6 > len(data) or data[offset:offset + 4] != b"PE\0\0":
    sys.exit(f"{path}: invalid PE header")
machine = struct.unpack_from("<H", data, offset + 4)[0]
if machine != expected:
    sys.exit(f"{path}: machine {machine:#06x}, expected {expected:#06x}")
PY

digest() {
  (cd "$(dirname -- "$1")" && sha256sum -- "$(basename -- "$1")" > "$(basename -- "$1").sha256")
}

mkdir -p "$out"
out="$(cd -- "$out" && pwd)"
bundle="$out/tilo-imager-windows-$arch"
rm -rf -- "$bundle"
mkdir -p "$bundle"
install -m 0644 "$exe" "$bundle/tilo-imager.exe"
install -m 0644 "$repo/docs/installer-walkthrough.md" "$bundle/README.md"
install -m 0644 "$repo/apps/imager/packaging/windows/Start-Tilo-Imager.ps1" "$bundle/Start-Tilo-Imager.ps1"
[ -z "$sign" ] || $sign "$bundle/tilo-imager.exe"

standalone="$out/tilo-imager-windows-$arch.exe"
cp -- "$bundle/tilo-imager.exe" "$standalone"
digest "$standalone"

# The same files in the same order with the same timestamps make the same zip.
zip="$out/tilo-imager-windows-$arch.zip"
rm -f -- "$zip"
python3 -I - "$bundle" "$zip" <<'PY'
import os, sys, zipfile
bundle, target = sys.argv[1], sys.argv[2]
with zipfile.ZipFile(target, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
    for name in sorted(os.listdir(bundle)):
        info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
        info.compress_type = zipfile.ZIP_DEFLATED
        info.external_attr = 0o644 << 16
        with open(os.path.join(bundle, name), "rb") as source:
            archive.writestr(info, source.read())
PY
digest "$zip"

[ -f "$iscc" ] || { printf 'Inno Setup compiler not found: %s\n' "$iscc" >&2; exit 1; }
command -v wine >/dev/null || { printf 'wine is required to run Inno Setup\n' >&2; exit 1; }
prefix="$(mktemp -d "${TMPDIR:-/tmp}/tilo-wine.XXXXXX")"
trap 'wineserver -k >/dev/null 2>&1 || true; rm -rf -- "$prefix"' EXIT
export WINEPREFIX="$prefix" WINEDEBUG="${WINEDEBUG:--all}" WINEDLLOVERRIDES="mscoree,mshtml="
bash "$repo/tools/ci/windows/wine-prefix.sh"
# Wine maps / to Z:, so Unix paths become Z:\ paths.
winpath() { printf 'Z:%s' "${1//\//\\}"; }
wine "$iscc" /Q "/DMyOut=$(winpath "$out")" "/DMyBundle=$(winpath "$bundle")" \
  "/DMyVersion=$version" "/DMyArch=$arch" "/DMyInstallerArch=$installer_arch" \
  "$(winpath "$repo/apps/imager/packaging/windows/tilo-imager.iss")"
installer="$out/tilo-imager-setup-windows-$arch.exe"
[ -s "$installer" ] || { printf 'Inno Setup produced no installer\n' >&2; exit 1; }
[ -z "$sign" ] || $sign "$installer"
digest "$installer"
ls -l -- "$out"
