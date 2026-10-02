#!/bin/bash
# Build the Orange Pi 3B U-Boot images that the mask ROM loads from the SD card.
#
# idbloader.img is the Rockchip TPL plus SPL. u-boot.itb is U-Boot proper.
# The TPL is Rockchip's DDR init blob, fetched and hash-checked here and never
# committed. BL31 is upstream Trusted Firmware-A. U-Boot is mainline, and its
# board description selects rk3566-orangepi-3b-v2.1 on a v2.1 board.
set -euo pipefail

out="${1:?usage: build-orangepi-uboot.sh OUTPUT_DIR}"
uboot_url=https://github.com/u-boot/u-boot/archive/refs/tags/v2026.07.tar.gz
uboot_sha256=21463d6d4498768d01fa192f6027468c04480d0daa99cd546f9a75a4d9ec353c
tfa_url=https://github.com/ARM-software/arm-trusted-firmware/archive/refs/tags/v2.14.0.tar.gz
tfa_sha256=d44936677a63c5216ffc151ae7711d458c992ef159a9df271fd9429f0a1cb5e5
ddr_url=https://github.com/rockchip-linux/rkbin/raw/master/bin/rk35/rk3566_ddr_1056MHz_v1.26.bin
ddr_sha256=9966c21aadf00737ccecc2fad6fe3c902ff26956433029f70dfd759d4f60616a

mkdir -p "$out"
out="$(cd "$out" && pwd)"
stamp="$out/pins.sha256"
expected="$(printf 'u-boot %s\ntfa %s\nddr %s\n' "$uboot_sha256" "$tfa_sha256" "$ddr_sha256")"
if [ -s "$out/idbloader.img" ] && [ -s "$out/u-boot.itb" ] && [ -f "$stamp" ] \
    && [ "$(cat "$stamp")" = "$expected" ]; then
  printf 'Orange Pi U-Boot already matches the pinned sources in %s\n' "$out"
  exit 0
fi

if [ "$(uname -m)" = aarch64 ]; then
  # Trusted Firmware-A treats an empty prefix as aarch64-none-elf-. That
  # tool's diagnostic is multi-line and breaks a makefile conditional, so the
  # native build names the host compiler. U-Boot parses a non-empty prefix as
  # a cross tuple and then fails to see that the host is aarch64, so it keeps
  # an empty prefix and uses the host gcc directly.
  tfa_cross=/usr/bin/
  uboot_cross=
  cc=gcc
else
  tfa_cross=aarch64-linux-gnu-
  uboot_cross=aarch64-linux-gnu-
  cc=aarch64-linux-gnu-gcc
fi
missing=""
for tool in "$cc" make dtc bison flex swig python3 bc; do
  command -v "$tool" >/dev/null || missing="$missing $tool"
done
python3 -c 'import elftools' >/dev/null 2>&1 || missing="$missing python3-pyelftools"
python3 -c 'import setuptools' >/dev/null 2>&1 || missing="$missing python3-setuptools"
# U-Boot's pylibfdt is a Python extension; it needs the interpreter's headers.
python3 -c 'import os, sysconfig; raise SystemExit(not os.path.exists(os.path.join(sysconfig.get_paths()["include"], "Python.h")))' \
  || missing="$missing python3-devel"
[ -f /usr/include/openssl/engine.h ] || missing="$missing openssl-devel-engine"
[ -f /usr/include/gnutls/gnutls.h ] || missing="$missing gnutls-devel"
if [ -n "$missing" ]; then
  printf 'Orange Pi U-Boot build needs:%s\n' "$missing" >&2
  exit 1
fi

src="${TILO_ORANGEPI_UBOOT_CACHE:-$out/src}"
mkdir -p "$src"
fetch() {
  local url="$1" dest="$2" digest="$3"
  if [ ! -s "$dest" ] || ! printf '%s  %s\n' "$digest" "$dest" | sha256sum --check --status; then
    curl -fL --retry 3 -o "$dest.partial" "$url"
    mv "$dest.partial" "$dest"
    printf '%s  %s\n' "$digest" "$dest" | sha256sum --check --status
  fi
}
fetch "$uboot_url" "$src/u-boot-v2026.07.tar.gz" "$uboot_sha256"
fetch "$tfa_url" "$src/arm-trusted-firmware-v2.14.0.tar.gz" "$tfa_sha256"
fetch "$ddr_url" "$src/rk3566_ddr_1056MHz_v1.26.bin" "$ddr_sha256"

work="$out/build"
rm -rf "$work"
mkdir -p "$work"
tar -xzf "$src/u-boot-v2026.07.tar.gz" -C "$work"
tar -xzf "$src/arm-trusted-firmware-v2.14.0.tar.gz" -C "$work"
make -C "$work/arm-trusted-firmware-2.14.0" CROSS_COMPILE="$tfa_cross" PLAT=rk3568 -j"$(nproc)" bl31
export BL31="$work/arm-trusted-firmware-2.14.0/build/rk3568/release/bl31/bl31.elf"
export ROCKCHIP_TPL="$src/rk3566_ddr_1056MHz_v1.26.bin"
make -C "$work/u-boot-2026.07" CROSS_COMPILE="$uboot_cross" orangepi-3b-rk3566_defconfig
make -C "$work/u-boot-2026.07" CROSS_COMPILE="$uboot_cross" -j"$(nproc)"
install -m 0644 "$work/u-boot-2026.07/idbloader.img" "$out/idbloader.img"
install -m 0644 "$work/u-boot-2026.07/u-boot.itb" "$out/u-boot.itb"
printf '%s\n' "$expected" > "$stamp"
printf 'Orange Pi U-Boot written to %s\n' "$out"
