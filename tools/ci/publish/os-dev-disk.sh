#!/usr/bin/env bash
# Main's dev image installed onto a disk, as `tilo-vm vm create` takes it and os-acceptance-ARCH
# boots it: never booted itself (they boot throwaway overlays of it), uncompressed so they read it
# fast (publication compresses it), following the dev channel of ARCH. As root in a micro-VM
# step. Nothing here holds a secret or publishes.
#
#   os-dev-disk.sh ARCH DEV OUT   DEV: os-dev-image-ARCH's layout; the disk into OUT/disk
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"

arch=${1:?usage: os-dev-disk.sh ARCH DEV OUT}
dev=${2:?}
out=${3:?}
[[ $(uname -m) == "$arch" ]] || die "the $arch image installs natively, not on $(uname -m)"
[[ -f $dev/oci/index.json ]] || die "$dev holds no image layout (oci/)"

missing=()
for p in dosfstools btrfs-progs systemd-container binutils qemu-img jq cryptsetup mtools zstd procps-ng; do
  rpm -q "$p" >/dev/null 2>&1 || missing+=("$p")
done
command -v podman >/dev/null || missing+=(podman skopeo fuse-overlayfs e2fsprogs util-linux python3)
((${#missing[@]} == 0)) || dnf install -y --setopt=install_weak_deps=False "${missing[@]}" >/dev/null
modprobe fuse 2>/dev/null || true
modprobe loop 2>/dev/null || true
use_cosign
rm -rf "$out"
mkdir -p "$out/disk"

revision=$(git rev-parse --verify HEAD) || die 'the disk names its commit: git = true'
id=$(podman pull -q "oci:$dev/oci")
[[ $(podman image inspect --format '{{.Digest}}' "$id") == "$(cat "$dev/digest")" ]] || die "$dev/oci is not the image $dev/digest names"
podman tag "$id" localhost/tilo-vm:ci
export TILO_DISTROBOX= TILO_VM_ARCH=$arch TMPDIR=/var/tmp TILO_USER=tester TILO_PASS=tilo
if [[ $arch == aarch64 ]]; then export TILO_DEV_DIRECT_BUNDLE_DIR=$out/disk/direct-boot; fi
TILO_IMAGE=localhost/tilo-vm:ci TILO_SKIP_BUILD=1 TILO_OUTPUT=$scratch/dev-disk TILO_RUN=$scratch/dev-disk-run \
  TILO_DISK_SIZE=64G TILO_TARGET_IMGREF="registry.tilo.so/tilo/tilo-vm:$arch-ci" \
  bash os/dev/build-image.sh > "$out/install.log" 2>&1 \
  || { tail -n 80 "$out/install.log"; die 'the dev image does not install onto a disk'; }
mv "$scratch/dev-disk/qcow2/disk.qcow2" "$out/disk/disk.qcow2"
printf '%s\n' "$revision" > "$out/disk/revision"
cp "$dev/digest" "$out/disk/image-digest"
printf 'installed %s of %s\n' "$(cat "$dev/digest")" "$revision"
