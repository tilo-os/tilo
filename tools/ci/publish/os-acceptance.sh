#!/usr/bin/env bash
# The OS image and its dev image as devices get them, as root in a micro-VM step with a
# /dev/kvm of its own. Nothing here holds a secret or publishes.
#
#   os-acceptance.sh ARCH IMAGE DEV OUT   IMAGE: os-image-ARCH's layout, DEV: os-dev-image-ARCH's;
#                                         reports and logs into OUT
#
# Without a VM, the OS image is held to its size budget (os/image-size-budget.toml). Then the dev
# image is installed onto a disk (os/dev/build-image.sh with TILO_SKIP_BUILD=1; it carries sshd
# and tilo-control, which the gates drive the guests with), kept in OUT/disk for
# publish-os-dev-ARCH, and three tracks run side by side, each booting throwaway overlays of that
# disk or the OS image's own UKI:
#
#   devices  a joined device signs in (os/dev/test-joined-device-startup.sh); nearby pairing,
#            a third device through one without the identity key, sibling unlock and, on
#            x86_64, wake (os/dev/test-nearby-pairing.sh); device-admin reach through a relay
#            (os/dev/test-device-admin-reach.sh)
#   desktop  the installed desktop from the sign-in screen on (os/dev/test-installed-desktop.sh)
#   splash   the boot splash on a panel larger than the firmware's mode (os/dev/check-boot-splash.py)
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"

arch=${1:?usage: os-acceptance.sh ARCH IMAGE DEV OUT}
image=${2:?}
dev=${3:?}
out=${4:?}
[[ $(uname -m) == "$arch" ]] || die "the $arch image is checked natively, not on $(uname -m)"
case $arch in
  x86_64) qemu=(edk2-ovmf qemu-system-x86-core qemu-device-display-virtio-vga qemu-device-display-virtio-gpu) ;;
  aarch64) qemu=(edk2-aarch64 qemu-system-aarch64-core qemu-device-display-virtio-gpu qemu-device-display-virtio-gpu-pci) ;;
  *) die "unknown architecture $arch" ;;
esac
for layout in "$image" "$dev"; do
  [[ -f $layout/oci/index.json ]] || die "$layout holds no image layout (oci/)"
done
[[ -c /dev/kvm ]] || die 'no /dev/kvm: the check needs requires = ["/dev/kvm"]'

missing=()
for p in dosfstools btrfs-progs systemd-container binutils socat qemu-img ipxe-roms-qemu sshpass \
  openssh-clients jq cryptsetup mtools zstd procps-ng "${qemu[@]}"; do
  rpm -q "$p" >/dev/null 2>&1 || missing+=("$p")
done
command -v podman >/dev/null || missing+=(podman skopeo fuse-overlayfs e2fsprogs util-linux python3)
((${#missing[@]} == 0)) || dnf install -y --setopt=install_weak_deps=False "${missing[@]}" >/dev/null
modprobe fuse 2>/dev/null || true
modprobe loop 2>/dev/null || true
use_cosign
rm -rf "$out"
mkdir -p "$out"
status=0

python3 tools/perf/image_size.py --oci-layout "$image/oci" --arch "$arch" --json "$out/image-size.json" --quiet \
  && python3 os/tests/image_size_budget.py check --flavour "production-$arch" --report "$out/image-size.json" \
  || { echo 'FAIL  the OS image is over its size budget'; status=1; }

# The disk publish-os-dev-ARCH publishes once every gate passed: never booted itself (the gates
# boot overlays of it), with zstd clusters, following the dev image it is published beside.
revision=$(git rev-parse --verify HEAD) || die 'the disk names its commit: git = true'
id=$(podman pull -q "oci:$dev/oci")
[[ $(podman image inspect --format '{{.Digest}}' "$id") == "$(cat "$dev/digest")" ]] || die "$dev/oci is not the image $dev/digest names"
podman tag "$id" localhost/tilo-vm:ci
export TILO_DISTROBOX= TILO_VM_ARCH=$arch TMPDIR=/var/tmp TILO_USER=tester TILO_PASS=tilo
mkdir -p "$out/disk"
if [[ $arch == aarch64 ]]; then export TILO_DEV_DIRECT_BUNDLE_DIR=$out/disk/direct-boot; fi
TILO_IMAGE=localhost/tilo-vm:ci TILO_SKIP_BUILD=1 TILO_OUTPUT=$scratch/dev-disk TILO_RUN=$scratch/dev-disk-run \
  TILO_DISK_SIZE=64G TILO_QCOW2_COMPRESS=1 TILO_TARGET_IMGREF="registry.tilo.so/tilo/tilo-vm:main-${revision:0:7}-ci" \
  bash os/dev/build-image.sh > "$out/install.log" 2>&1 \
  || { tail -n 80 "$out/install.log"; die 'the dev image does not install onto a disk'; }
unset TILO_DEV_DIRECT_BUNDLE_DIR
podman rmi -f localhost/tilo-vm:ci >/dev/null
disk=$out/disk/disk.qcow2
mv "$scratch/dev-disk/qcow2/disk.qcow2" "$disk"
printf '%s\n' "$revision" > "$out/disk/revision"
cp "$dev/digest" "$out/disk/image-digest"

# A host without a usable render node boots the guests on the 2D virtio display; the
# compositor paints on the CPU there, and every gate must pass either way.
export TILO_VM_DISPLAY=2d
for node in /dev/dri/renderD*; do
  [[ -c $node && -r $node && -w $node ]] && { TILO_VM_DISPLAY=gl; break; }
done

wake=()
if [[ $arch == x86_64 ]]; then wake=(--wake); fi
devices() {
  local rc=0
  bash os/dev/test-joined-device-startup.sh "$disk" --out "$out/joined-device" || rc=1
  bash os/dev/test-nearby-pairing.sh "$disk" --out "$out/nearby-pairing" --unlock "${wake[@]}" || rc=1
  bash os/dev/test-device-admin-reach.sh "$disk" --out "$out/device-admin-reach" || rc=1
  return "$rc"
}
desktop() {
  bash os/dev/test-installed-desktop.sh "$disk" --out "$out/installed-desktop" --base-digest "$(cat "$image/digest")"
}
splash() {
  python3 os/dev/check-boot-splash.py --arch "$arch" --out "$out/boot-splash" "oci:$image/oci:tilo"
}
tracks=(devices desktop splash)
pids=()
for track in "${tracks[@]}"; do
  ( set -o pipefail; "$track" 2>&1 | tee "$out/$track.log" | sed -u "s/^/[$track] /" ) &
  pids+=("$!")
done
for i in "${!tracks[@]}"; do
  if wait "${pids[$i]}"; then
    echo "ok    ${tracks[$i]}"
  else
    echo "FAIL  ${tracks[$i]} (its log: $out/${tracks[$i]}.log)"
    status=1
  fi
done
exit "$status"
