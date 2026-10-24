#!/usr/bin/env bash
# A broken update falls back by itself, proved on this tree's own dev image
# booted under KVM, as t's boot-assessment-x86_64 check runs it (t.toml).
#
# The disk is this tree's dev image (os-dev-image-x86_64, which the check
# needs), installed as vm-acceptance.sh installs it. The update staged on it is
# that same image with one more layer, so bootc sees a different deployment.
# os/tests/boot_assessment_rollback.sh then stages it, breaks only it and
# reboots until the boot loader has spent its tries and gone back.
#
# Inputs (t dispatch boot-assessment-x86_64 --input fault=...; every other run
# gets the default):
#   fault   unlock (default): the update's boot waits at a disk unlock prompt
#           nobody answers and is power-cycled there; greetd or greeter: it
#           boots but never reaches a working greeter
#
# The record of every boot lands in /target/boot-assessment.
set -euo pipefail

arch=x86_64
work=/var/tmp/boot-assessment
out=${CARGO_TARGET_DIR:-/target}/boot-assessment
dev=/target/publish/os-dev/$arch
fault=${T_INPUT_FAULT:-unlock}
case $fault in unlock|greetd|greeter) ;; *) echo "fault must be unlock, greetd or greeter" >&2; exit 2 ;; esac

dnf --setopt=retries=3 --setopt=install_weak_deps=False install -y \
  qemu-kvm qemu-img edk2-ovmf ipxe-roms-qemu openssh-clients sshpass socat \
  btrfs-progs dosfstools e2fsprogs fuse-overlayfs podman jq skopeo systemd util-linux
command -v qemu-system-x86_64 qemu-img ssh sshpass socat podman systemd-repart >/dev/null

./os/dev/assert-native-arch.sh "$arch"
python3 os/dev/probe-kvm.py "$arch" || {
  echo 'this node cannot create a KVM VM; boots under TCG are too slow to count' >&2
  exit 1
}

rm -rf "$work" "$out"
mkdir -p "$work" "$out"
target=$(git rev-parse HEAD)
[[ -s $dev/digest && -s $dev/revision && -d $dev/oci ]] \
  || { echo "no dev image in $dev: the check needs os-dev-image-$arch" >&2; exit 1; }
commit=$(cat "$dev/revision")
[[ $commit == "$target" ]] || { echo "the dev image is of $commit, this tree is $target" >&2; exit 1; }

modprobe fuse 2>/dev/null || true
modprobe loop 2>/dev/null || true
test -c /dev/fuse && test -c /dev/loop-control
losetup --find >/dev/null
mkdir -p "$work/install" "$work/install-run"
image=$(podman pull -q "oci:$dev/oci")
[[ $(podman image inspect --format '{{.Digest}}' "$image") == "$(cat "$dev/digest")" ]] \
  || { echo "$dev/oci is not the image $dev/digest names" >&2; exit 1; }
podman tag "$image" localhost/tilo-vm:ci
TILO_IMAGE=localhost/tilo-vm:ci TILO_SKIP_BUILD=1 TILO_DISTROBOX="" \
  TILO_OUTPUT=$work/install TILO_RUN=$work/install-run TILO_DISK_SIZE=64G \
  TILO_TARGET_IMGREF="registry.tilo.so/tilo/tilo-vm:main-${commit:0:7}-ci" ./os/dev/build-image.sh
disk=$work/disk.qcow2
mv "$work/install/qcow2/disk.qcow2" "$disk"

# The update: the same image and one more layer, a different deployment.
# From a Containerfile in the context: podman resolves `-f -` through /dev/stdin,
# which t's micro-VMs do not have.
mkdir -p "$work/update"
printf 'FROM localhost/tilo-vm:ci\nRUN echo %s > /usr/lib/tilo/boot-assessment-update\n' "$target" \
  > "$work/update/Containerfile"
podman build -q -t localhost/tilo-boot-assessment:update -f "$work/update/Containerfile" "$work/update" >/dev/null
podman save --format oci-archive -o "$work/update.tar" localhost/tilo-boot-assessment:update
podman rmi -f localhost/tilo-boot-assessment:update localhost/tilo-vm:ci "$image" >/dev/null 2>&1 || true

export TILO_DISTROBOX="" TILO_ACCEL=kvm TILO_USER=quickuser TILO_PASS=tilo
export TILO_RUN=$work/run TILO_DISK=$disk
mkdir -p "$TILO_RUN"
finish() {
  ./os/dev/vm.sh serial > "$out/serial.log" 2>&1 || true
  ./os/dev/vm.sh stop >/dev/null 2>&1 || true
}
trap finish EXIT

./os/dev/vm.sh boot --2d --disk "$disk"
for _ in $(seq 1 180); do ./os/dev/vm.sh ssh true >/dev/null 2>&1 && break; sleep 2; done
./os/dev/vm.sh ssh true >/dev/null || { echo 'the installed disk did not accept SSH' >&2; exit 1; }

sshpass -p "$TILO_PASS" scp -q -P "${TILO_SSH_PORT:-2222}" -F /dev/null \
  -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
  -o PreferredAuthentications=password -o PubkeyAuthentication=no \
  "$work/update.tar" "$TILO_USER@127.0.0.1:/var/tmp/boot-assessment-update.tar"
./os/dev/vm.sh ssh "printf '%s\n' '$TILO_PASS' | sudo -S -p '' sh -c \
  'podman load -q -i /var/tmp/boot-assessment-update.tar && rm -f /var/tmp/boot-assessment-update.tar'"
rm -f "$work/update.tar"

RUNNER='./os/dev/vm.sh ssh' SUDO_PASS="$TILO_PASS" FAULT="$fault" \
  RESET="./os/dev/vm.sh stop; ./os/dev/vm.sh boot --2d --disk '$disk'" \
  SERIAL='./os/dev/vm.sh serial' \
  ./os/tests/boot_assessment_rollback.sh containers-storage:localhost/tilo-boot-assessment:update "$out"
