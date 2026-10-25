#!/usr/bin/env bash
# What a development device needs of main's dev image before it follows it: it installs, boots
# to a greeter that takes input, which the device's own boot assessment (tilo-boot-health)
# requires before it keeps a new image instead of falling back, and takes the
# next update: bootc stages a newer image and `tilo-extension live:system` applies it.
# As root in a micro-VM step with a /dev/kvm of its own. Nothing here holds a secret or publishes.
#
#   os-dev-gate.sh ARCH DEV OUT   DEV: os-dev-image-ARCH's layout; the disk and logs into OUT
#
# The dev image is installed onto a disk (os/dev/build-image.sh with TILO_SKIP_BUILD=1), kept in
# OUT/disk for publish-os-dev-ARCH and os-acceptance-ARCH, which boot throwaway overlays of it.
# This is the dev channel's whole gate: a device falls back by itself from an image that does
# not reach a working greeter, so a failure the gate misses costs a dev device a reboot, not its
# data, as long as every published image can boot and take the update that fixes it.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"

arch=${1:?usage: os-dev-gate.sh ARCH DEV OUT}
dev=${2:?}
out=${3:?}
[[ $(uname -m) == "$arch" ]] || die "the $arch image is checked natively, not on $(uname -m)"
case $arch in
  x86_64) qemu=(edk2-ovmf qemu-system-x86-core qemu-device-display-virtio-vga qemu-device-display-virtio-gpu) ;;
  aarch64) qemu=(edk2-aarch64 qemu-system-aarch64-core qemu-device-display-virtio-gpu qemu-device-display-virtio-gpu-pci) ;;
  *) die "unknown architecture $arch" ;;
esac
[[ -f $dev/oci/index.json ]] || die "$dev holds no image layout (oci/)"
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
mkdir -p "$out/disk"

# The disk: never booted itself (the gates boot overlays of it), uncompressed so they read it
# fast (publication compresses it), following the dev image it is published beside.
revision=$(git rev-parse --verify HEAD) || die 'the disk names its commit: git = true'
id=$(podman pull -q "oci:$dev/oci")
[[ $(podman image inspect --format '{{.Digest}}' "$id") == "$(cat "$dev/digest")" ]] || die "$dev/oci is not the image $dev/digest names"
podman tag "$id" localhost/tilo-vm:ci
export TILO_DISTROBOX= TILO_VM_ARCH=$arch TMPDIR=/var/tmp TILO_USER=tester TILO_PASS=tilo
if [[ $arch == aarch64 ]]; then export TILO_DEV_DIRECT_BUNDLE_DIR=$out/disk/direct-boot; fi
TILO_IMAGE=localhost/tilo-vm:ci TILO_SKIP_BUILD=1 TILO_OUTPUT=$scratch/dev-disk TILO_RUN=$scratch/dev-disk-run \
  TILO_DISK_SIZE=64G TILO_TARGET_IMGREF="registry.tilo.so/tilo/tilo-vm:main-${revision:0:7}-ci" \
  bash os/dev/build-image.sh > "$out/install.log" 2>&1 \
  || { tail -n 80 "$out/install.log"; die 'the dev image does not install onto a disk'; }
unset TILO_DEV_DIRECT_BUNDLE_DIR
# The update: the dev image with one file added, served by the registry the install left
# running, which the guest reaches as the host (10.0.2.2).
probe=/usr/share/tilo/live-update-probe
probe_text="live update of ${revision:0:12}"
mkdir -p "$scratch/live-update"
printf '%s\n' "$probe_text" > "$scratch/live-update/probe"
printf 'FROM localhost/tilo-vm:ci\nCOPY probe %s\n' "$probe" > "$scratch/live-update/Containerfile"
podman build -q -t localhost/tilo-vm:live-update-check "$scratch/live-update" >/dev/null
registry_port=$(podman port tilo-disk-registry 5000/tcp | head -n 1)
registry_port=${registry_port##*:}
[[ $registry_port =~ ^[0-9]+$ ]] || die 'the install left no local registry for the update'
skopeo copy -q --dest-tls-verify=false containers-storage:localhost/tilo-vm:live-update-check \
  "docker://localhost:$registry_port/tilo-vm:live-update-check"
podman rmi -f localhost/tilo-vm:ci localhost/tilo-vm:live-update-check >/dev/null
disk=$out/disk/disk.qcow2
mv "$scratch/dev-disk/qcow2/disk.qcow2" "$disk"
printf '%s\n' "$revision" > "$out/disk/revision"
cp "$dev/digest" "$out/disk/image-digest"

export TILO_VM_DISPLAY=2d
for node in /dev/dri/renderD*; do
  [[ -c $node && -r $node && -w $node ]] && { TILO_VM_DISPLAY=gl; break; }
done
vm=os/dev/vm.sh
export TILO_RUN=/var/tmp/tilo-dev-gate-$arch TILO_SSH_PORT=2222
export TILO_DISK=$TILO_RUN/disk.qcow2
rm -rf -- "$TILO_RUN"
install -d -m 0700 "$TILO_RUN"
finish() {
  local rc=$?
  trap - EXIT
  if ((rc != 0)); then
    TILO_SSH_BOUND=60 "$vm" ssh 'systemctl --failed --no-pager; journalctl -b --no-pager -o short-monotonic -p warning -n 200' || true
    "$vm" serial 2>/dev/null | tail -n 80 || true
  fi
  "$vm" stop >/dev/null 2>&1 || true
  cp -f "$TILO_RUN/serial.log" "$out/" 2>/dev/null || true
  rm -rf -- "$TILO_RUN"
  exit "$rc"
}
trap finish EXIT
"$vm" tool qemu-img create -f qcow2 -F qcow2 -b "$disk" "$TILO_DISK" >/dev/null
"$vm" boot "--$TILO_VM_DISPLAY"
"$vm" wait
# The device's own boot assessment, which decides whether a boot is kept or falls back:
# boot-complete.target pulls it in only on a counted boot entry, which a fresh install has none of.
TILO_SSH_BOUND=780 "$vm" ssh "printf '%s\\n' '$TILO_PASS' | sudo -S -p '' systemctl start tilo-boot-health.service" \
  || die 'tilo-boot-health failed: a device would fall back from this image'
echo 'ok    boots to a greeter that takes input, as the boot assessment requires'
"$vm" with-tests "printf '%s\\n' '$TILO_PASS' | sudo -S -p '' \"\$TILO_GUEST_TESTS/live-update-check.sh\" '10.0.2.2:$registry_port/tilo-vm' registry.tilo.so/tilo/tilo-vm:live-update-check '$probe' '$probe_text'"
echo 'ok    stages the next image and applies it live'
