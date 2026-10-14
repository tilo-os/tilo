#!/usr/bin/env bash
# Pack the Windows compatibility payload as a systemd-sysext image.
#
#   build-windows-compat-sysext.sh TREE OUT.raw
#
# TREE holds only /usr content (the FEX payload, the x86 guest root, Proton, the
# Steam runtime, the virtio-win media). The image overlays those exact paths on
# the sealed /usr, so every hard-coded path keeps working once it is merged. It
# is a bare read-only file system, no partition table, named
# tilo-windows-compat.raw so systemd-sysext reads
# usr/lib/extension-release.d/extension-release.tilo-windows-compat from it and
# refuses it on any other OS release or architecture.
#
# erofs when mkfs.erofs exists (smaller download), otherwise ext4. Prints the
# descriptor the publisher pins: {"part","file","size","sha256","fs"}.
set -euo pipefail

tree=${1:?usage: build-windows-compat-sysext.sh TREE OUT.raw}
out=${2:?usage: build-windows-compat-sysext.sh TREE OUT.raw}
part=tilo-windows-compat
[[ -d $tree/usr ]] || { echo "build-windows-compat-sysext: $tree/usr is missing" >&2; exit 1; }
[[ $(basename "$out") == "$part.raw" ]] || { echo "build-windows-compat-sysext: the image must be named $part.raw" >&2; exit 1; }
# Anything outside /usr would be silently ignored by systemd-sysext.
extra=$(find "$tree" -mindepth 1 -maxdepth 1 ! -name usr -printf '%f ')
[[ -z $extra ]] || { echo "build-windows-compat-sysext: only /usr may travel, found: $extra" >&2; exit 1; }

os_release=${TILO_SYSEXT_OS_RELEASE:-/usr/lib/os-release}
# shellcheck disable=SC1090
id=$(. "$os_release"; printf '%s' "$ID")
version_id=$(. "$os_release"; printf '%s' "$VERSION_ID")
case "${TILO_SYSEXT_ARCH:-$(uname -m)}" in
  x86_64) architecture=x86-64 ;;
  aarch64) architecture=arm64 ;;
  *) echo "build-windows-compat-sysext: unsupported architecture" >&2; exit 1 ;;
esac
release_dir=$tree/usr/lib/extension-release.d
mkdir -p "$release_dir"
printf 'ID=%s\nVERSION_ID=%s\nARCHITECTURE=%s\nSYSEXT_SCOPE=system\nTILO_EXTENSION=windows-compat\n' \
  "$id" "$version_id" "$architecture" > "$release_dir/extension-release.$part"
chmod 0644 "$release_dir/extension-release.$part"

mkdir -p "$(dirname "$out")"
rm -f -- "$out"
if command -v mkfs.erofs >/dev/null; then
  fs=erofs
  mkfs.erofs -zzstd -T0 -U clear --all-root "$out" "$tree" >&2
else
  fs=ext4
  bytes=$(du -sb --apparent-size "$tree" | cut -f1)
  # Slack for ext4 metadata; the inode count follows the tree.
  truncate -s $(( bytes + bytes / 16 + 128 * 1024 * 1024 )) "$out"
  mkfs.ext4 -F -q -m 0 -O ^has_journal -L tilo-wincompat -E root_owner=0:0 -d "$tree" "$out" >&2
fi
size=$(stat -c %s "$out")
sha256=$(sha256sum "$out" | cut -d' ' -f1)
printf '{"schema":1,"part":"%s","file":"%s.raw","size":%s,"sha256":"%s","fs":"%s"}\n' \
  "$part" "$part" "$size" "$sha256" "$fs"
