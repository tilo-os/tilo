#!/usr/bin/env bash
# Pack the Windows compatibility payload as a systemd-sysext image.
#
#   build-windows-compat-sysext.sh TREE OUT.raw
#
# TREE holds only /usr content (the FEX payload, the x86 guest root, Proton on
# x86, the virtio-win media). The image overlays those exact paths on
# the sealed /usr, so every hard-coded path keeps working once it is merged. It
# is a bare read-only file system, no partition table, named
# tilo-windows-compat.raw so systemd-sysext reads
# usr/lib/extension-release.d/extension-release.tilo-windows-compat from it and
# refuses it on any other OS release or architecture.
#
# Merged, the image's own directories stand in for /usr's (the overlay shows the
# top layer's owner, mode and label for a directory both have): an unlabelled or
# user-owned /usr locks every login out and stops the boot in
# systemd-tmpfiles-setup. So every entry is owned by root and labelled from the
# base's SELinux file contexts, directories the base has keep its modes, and
# sysext-image.py check refuses an image that would change any of that.
#
# The image is ext4: the build's package set has no mkfs.erofs. Prints the
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

contexts=${TILO_SYSEXT_FILE_CONTEXTS:-/etc/selinux/targeted/contexts/files/file_contexts}
[[ -f $contexts ]] || { echo "build-windows-compat-sysext: no SELinux file contexts at $contexts" >&2; exit 1; }
# Upstream archives carry their builders' owners and modes: everything is root's,
# and nothing under /usr is writable by anyone but its owner or takes on another
# identity.
chown -hR 0:0 "$tree"
find "$tree" ! -type l -perm /6022 -exec chmod go-w,ug-s {} +
# A directory the base has keeps the base's mode, since the merged image's
# directory stands in for it.
base=${TILO_SYSEXT_BASE:-/}
while IFS= read -r -d '' dir; do
  if [[ -d $base/$dir && ! -L $base/$dir ]]; then chmod --reference="$base/$dir" "$tree/$dir"; fi
done < <(cd "$tree" && find usr -type d -print0)

mkdir -p "$(dirname "$out")"
rm -f -- "$out"
fs=ext4
bytes=$(du -sb --apparent-size "$tree" | cut -f1)
# Slack for ext4 metadata; the inode count follows the tree. 256-byte inodes keep
# each label inside its inode.
truncate -s $(( bytes + bytes / 16 + 128 * 1024 * 1024 )) "$out"
mkfs.ext4 -F -q -m 0 -I 256 -O ^has_journal -L tilo-wincompat -E root_owner=0:0 -d "$tree" "$out" >&2
here=$(dirname -- "${BASH_SOURCE[0]}")
python3 -I "$here/sysext-image.py" label "$out" "$tree" --contexts "$contexts" >&2
python3 -I "$here/sysext-image.py" check "$out" --base "$base" >&2
size=$(stat -c %s "$out")
sha256=$(sha256sum "$out" | cut -d' ' -f1)
printf '{"schema":1,"part":"%s","file":"%s.raw","size":%s,"sha256":"%s","fs":"%s"}\n' \
  "$part" "$part" "$size" "$sha256" "$fs"
