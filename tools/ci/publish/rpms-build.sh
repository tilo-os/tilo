#!/usr/bin/env bash
# The build half of the frozen RPM snapshot (os/Containerfile.rpms) for this node's
# architecture: every package the OS Containerfiles name, resolved from Fedora, signature-checked
# and laid out as a repository image, into OUT (oci-build.sh). Holds no secrets.
#
# usage: rpms-build.sh ARCH OUT
#
# OUT/epoch is the snapshot's clock: the newest signature among its packages (snapshot_epoch).
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/build-lib.sh"
arch=${1:?usage: rpms-build.sh ARCH OUT}
out=${2:?}
case $arch in
  x86_64) platform=linux/amd64 ;;
  aarch64) platform=linux/arm64 ;;
  *) die "unknown architecture $arch" ;;
esac
[[ $(uname -m) == "$arch" ]] || die "the $arch snapshot is built natively, not on $(uname -m)"
cd "$repo_root"
. os/base-snapshot.env
packages=$(python3 os/dev/derive-rpm-package-set.py --arch "$arch" | tr '\n' ' ')
[[ -n ${packages// /} ]] || die "no packages for $arch"
# The newest signature among the packages of the snapshot archive $1, as seconds: the clock an OS
# build from the unpublished snapshot takes (with its base's date), which rpm refuses a package
# signed after. The image itself is dated when it was resolved, which a build of the same packages
# minutes later moves, and with it every layer built on it; this moves only with the packages.
snapshot_epoch() {
  local dir blob epoch
  dir=$(mktemp -d "$scratch/snapshot.XXXXXX")
  mkdir "$dir/oci" "$dir/root"
  tar -C "$dir/oci" -xf "$1"
  for blob in "$dir"/oci/blobs/sha256/*; do
    [[ $(head -c 2 "$blob" | od -An -tx1 | tr -d ' \n') == 1f8b ]] || continue
    tar -C "$dir/root" -xzf "$blob"
  done
  epoch=$(LC_ALL=C TZ=UTC rpm -qp --nosignature --nodigest --qf '%{RSAHEADER:pgpsig}\n' "$dir"/root/repo/*.rpm \
    | sed -n 's/^[^,]*, \(.*\), Key ID .*/\1/p' | TZ=UTC date -f - +%s | sort -n | tail -n 1)
  rm -rf -- "$dir"
  [[ $epoch =~ ^[1-9][0-9]*$ ]] || die "no signed package in $1 to date the snapshot by"
  printf '%s\n' "$epoch"
}

assets=$(pinned_assets virtio-win-0.1.302-1.noarch.rpm)
# The image is dated when its packages were resolved, not at oci-build.sh's fixed epoch (a
# later --timestamp wins): an OS build from the published snapshot sets its offline dnf clock to
# the newest of its base's and its snapshot's dates (os/dev/image-epoch.sh), and rpm refuses a
# package signed after that clock, as every update signed since the base was mirrored is. A build
# from this unpublished one takes OUT/epoch instead.
bash tools/ci/publish/oci-build.sh . os/Containerfile.rpms "$out" --platform "$platform" \
  --timestamp "$(date -u +%s)" --build-context "github-assets=$assets" \
  --build-arg "BASE_REF=$BASE_REF" --build-arg "PACKAGES=$packages"
snapshot_epoch "$out/image.oci.tar" > "$out/epoch"
printf 'dated %s by its newest package signature\n' "$(cat "$out/epoch")"
