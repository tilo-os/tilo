#!/usr/bin/env bash
# The OS image os-image.sh built, checked as a device gets it, as root in a micro-VM step with a
# /dev/kvm of its own. Nothing here holds a secret or publishes.
#
#   os-image-smoke.sh ARCH IN OUT   the image `os-image.sh build` left in IN; logs into OUT
#
# First the finished tree, inside the image and without a network (tools/ci/image_smoke.py).
# Then the image is installed onto a disk the way a release validation installs one
# (os/dev/build-image.sh with TILO_SKIP_BUILD=1: the two-pass composefs seal, then the qcow2)
# and booted under KVM with the repository's VM harness (os/dev/vm.sh) until the greeter accepts
# input on its console channel. A unit that fails on the way prints `[FAILED]` on the serial
# console, where the sealed command line sends the console; any such line fails the boot.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"

arch=${1:?usage: os-image-smoke.sh ARCH IN OUT}
in=${2:?}
out=${3:?}
[[ $(uname -m) == "$arch" ]] || die "the $arch image is checked natively, not on $(uname -m)"
case $arch in
  x86_64) qemu=(edk2-ovmf qemu-system-x86-core qemu-device-display-virtio-vga) ;;
  aarch64) qemu=(edk2-aarch64 qemu-system-aarch64-core qemu-device-display-virtio-gpu-pci) ;;
  *) die "unknown architecture $arch" ;;
esac
# The OS image as os-image-<arch> composed it: an OCI layout named by its digest, whichever
# commit's run composed it from the same layers.
[[ -f $in/oci/index.json ]] || die "$in holds no OS image layout (oci/)"
[[ -c /dev/kvm ]] || die 'no /dev/kvm: the check needs requires = ["/dev/kvm"]'

# Root's tools in the guest: podman for the image, what disk-image.sh installs with, and QEMU.
# The OS assembler image has podman and the verification tools; QEMU and the disk tools come
# from the frozen snapshot where the image lacks them.
missing=()
for p in dosfstools btrfs-progs systemd-container binutils socat qemu-img ipxe-roms-qemu "${qemu[@]}"; do
  rpm -q "$p" >/dev/null 2>&1 || missing+=("$p")
done
command -v podman >/dev/null || missing+=(podman skopeo fuse-overlayfs e2fsprogs util-linux jq python3)
((${#missing[@]} == 0)) || dnf install -y --setopt=install_weak_deps=False "${missing[@]}" >/dev/null
modprobe fuse 2>/dev/null || true
modprobe loop 2>/dev/null || true
use_cosign
id=$(podman pull -q "oci:$in/oci")
[[ $(podman image inspect --format '{{.Digest}}' "$id") == "$(cat "$in/digest")" ]] || die "$in/oci is not the image $in/digest names"
podman tag "$id" localhost/tilo:ci
mkdir -p "$out"
status=0

podman run --rm --network=none --security-opt label=disable \
  -v "$repo_root/tools/ci/image_smoke.py:/run/image-smoke.py:ro" localhost/tilo:ci \
  python3 -I -B /run/image-smoke.py 2>&1 | tee "$out/image-smoke.log" || status=1

export TILO_IMAGE=localhost/tilo:ci TILO_SKIP_BUILD=1 TILO_DISTROBOX= TILO_VM_ARCH=$arch \
  TILO_OUTPUT=$scratch/boot-output TILO_RUN=$scratch/boot-run TILO_MEM=6144 TILO_SMP=4 \
  TILO_BOOT_WAIT_ATTEMPTS=150 TMPDIR=/var/tmp
mkdir -p "$TILO_OUTPUT" "$TILO_RUN"
boot() {
  bash os/dev/build-image.sh || { echo 'FAIL  the image does not install onto a disk'; return 1; }
  export TILO_DISK=$TILO_OUTPUT/qcow2/disk.qcow2
  bash os/dev/vm.sh boot --2d || { echo 'FAIL  the VM does not start'; return 1; }
  local booted=0
  bash os/dev/vm.sh wait && booted=1
  bash os/dev/vm.sh stop >/dev/null 2>&1 || true
  tr -d '\000\r' < "$TILO_RUN/serial.log" | sed 's/\x1b\[[0-9;]*[A-Za-z]//g' > "$out/serial.log" || true
  if (( booted )); then
    echo 'ok    the installed image boots to a greeter that accepts input'
  else
    echo 'FAIL  the installed image does not boot to a greeter that accepts input'
  fi
  if grep -aq 'FAILED' "$out/serial.log"; then
    echo 'FAIL  units failed while it booted:'
    grep -a 'FAILED' "$out/serial.log" | sed 's/^/        /'
    booted=0
  elif (( booted )); then
    echo 'ok    no unit failed while it booted'
  fi
  (( booted )) || { tail -n 80 "$out/serial.log" | sed 's/^/  serial: /'; return 1; }
}
boot 2>&1 | tee "$out/boot.log" || status=1
exit "$status"
