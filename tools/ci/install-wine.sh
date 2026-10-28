#!/usr/bin/env bash
# Build Wine from its pinned upstream source with Tilo's patches
# (os/external-assets/wine/README.md) and install it under DESTDIR/usr/local.
# The x86_64 CI builder (os/Containerfile.builder) runs Inno Setup and the
# Windows test executables in it.
#
# The caller owns fetching the source into the asset cache and installing the
# build dependencies (gcc, make, flex, bison, patch, mingw32-gcc, mingw64-gcc,
# freetype-devel, fontconfig-devel, gnutls-devel). At runtime Wine needs
# freetype, fontconfig and gnutls.
#
#   install-wine.sh CACHE DESTDIR ASSET_MANIFEST PATCH_DIR
#   install-wine.sh --asset-name
set -euo pipefail

version=11.20
asset=wine-$version.tar.xz

usage() {
  echo 'usage: install-wine.sh CACHE DESTDIR ASSET_MANIFEST PATCH_DIR' >&2
  echo '       install-wine.sh --asset-name' >&2
  exit 2
}

if [[ $# == 1 && $1 == --asset-name ]]; then
  printf '%s\n' "$asset"
  exit 0
fi
[[ $# == 4 ]] || usage
cache=$1
destdir=$2
manifest=$3
patches=$(cd "$4" && pwd)
[[ $destdir == /* ]] || { echo 'Wine destination must be absolute' >&2; exit 2; }
expected=$(awk -F '\t' -v name="$asset" '$1 !~ /^#/ && $2 == name {print $1}' "$manifest")
[[ $expected =~ ^[0-9a-f]{64}$ ]] || { echo "missing or ambiguous digest for $asset" >&2; exit 1; }
printf '%s  %s\n' "$expected" "$asset" | (cd "$cache" && sha256sum --check --strict -) >&2

stage=$(mktemp -d)
trap 'rm -rf -- "$stage"' EXIT
tar -xJf "$cache/$asset" --strip-components=1 --no-same-owner -C "$stage"
for patch in "$patches"/*.patch; do
  patch -d "$stage" -p1 --forward --quiet < "$patch"
done
# WoW64 with both PE architectures: Inno Setup is a 32-bit program, the test
# executables 64-bit. Nothing here has a display, sound or devices; the
# programs get Wine's null graphics driver (tools/ci/windows/wine-prefix.sh).
# No debug information and no development files: the build is to run
# programs, and both would more than double its size.
(
  cd "$stage"
  ./configure --quiet --prefix=/usr/local --enable-archs=i386,x86_64 --disable-tests \
    CFLAGS=-O2 CROSSCFLAGS=-O2 \
    --without-alsa --without-capi --without-cups --without-dbus --without-ffmpeg \
    --without-gssapi --without-gstreamer --without-krb5 --without-netapi --without-opencl \
    --without-opengl --without-oss --without-pcap --without-pcsclite --without-pulse \
    --without-sane --without-sdl --without-udev --without-usb --without-v4l2 \
    --without-vulkan --without-wayland --without-x
  make --silent -j"$(nproc)"
  make --silent install-lib DESTDIR="$destdir"
)
licenses="$destdir/usr/local/share/licenses/tilo-wine"
install -D -m 0644 -t "$licenses" "$stage/LICENSE" "$stage/COPYING.LIB" "$patches"/*.patch
printf '%s\n' \
  'Wine - Windows compatibility layer' \
  'License: LGPL-2.1-or-later (see LICENSE, COPYING.LIB)' \
  "Source: https://dl.winehq.org/wine/source/${version%%.*}.x/$asset" \
  'Modified: built from the source above with the patches here.' \
  > "$licenses/NOTICE"
"$destdir/usr/local/bin/wine" --version
