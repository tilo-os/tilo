#!/usr/bin/env bash
# The server image proves itself by booting, in a VM: the machine comes up
# serving (the hosted-VM service binds its socket, the boot judges itself, an
# update stages through the verified path), and an update that breaks the server
# rolls itself back without anyone at the machine.
#
# The disk is the image the check image-host built, installed the way the
# desktop's dev disk is (os/dev/build-image.sh, bootc install to-filesystem).
# The update staged on it is the same image with one more layer, so bootc sees a
# different deployment; os/tests/boot_assessment_rollback.sh then stages it,
# breaks only it and reboots until the boot loader has spent its tries and gone
# back. The server image runs no sshd, so every step is driven at the serial
# console, which is also how a person reaches it (os/tests/serial_runner.sh).
#
# The record of every boot lands in /target/host-assurance.
set -euo pipefail

arch=x86_64
work=/var/tmp/host-assurance
out=${CARGO_TARGET_DIR:-/target}/host-assurance
pub=/target/publish/host
update_ref=localhost/tilo-host:assurance-update

# Where the disk's account comes from, and what the console runner signs in as
# (os/account.env: a development credential for disks built for tests).
. os/account.env
TILO_USER=${TILO_ACCOUNT%%:*}
rest=${TILO_ACCOUNT#*:}
TILO_PASS=${rest%%:*}

say() { printf '\n=== %s\n' "$*" >&2; }
die() { printf 'host-assurance: %s\n' "$*" >&2; exit 1; }

bash tools/ci/install-packages.sh tools/ci/packages/boot-assessment.txt
command -v qemu-system-x86_64 qemu-img podman skopeo systemd-repart >/dev/null
./os/dev/assert-native-arch.sh "$arch"
python3 os/dev/probe-kvm.py "$arch" || {
  echo 'this node cannot create a KVM VM; boots under TCG are too slow to count' >&2
  exit 1
}

rm -rf "$work" "$out"
mkdir -p "$work" "$out"
target=$(git rev-parse HEAD)
[[ -s $pub/image.oci.tar && -s $pub/digest ]] \
  || die "no server image in $pub: the check needs image-host"

modprobe fuse 2>/dev/null || true
modprobe loop 2>/dev/null || true
test -c /dev/fuse || die 'no /dev/fuse, which the install needs'
test -c /dev/loop-control || die 'no loop devices, which the install needs'
losetup --find >/dev/null

say 'install the server image'
image=$(podman pull -q "oci-archive:$pub/image.oci.tar")
[[ $(podman image inspect --format '{{.Digest}}' "$image") == "$(cat "$pub/digest")" ]] \
  || die "$pub/image.oci.tar is not the image $pub/digest names"
podman tag "$image" localhost/tilo-host:ci
TILO_IMAGE=localhost/tilo-host:ci TILO_SKIP_BUILD=1 TILO_DISTROBOX="" \
  TILO_OUTPUT=$work/install TILO_RUN=$work/install-run TILO_DISK_SIZE=64G \
  TILO_TARGET_IMGREF="registry.tilo.so/tilo/tilo-host:main-${target:0:7}-ci" ./os/dev/build-image.sh
disk=$work/disk.qcow2
mv "$work/install/qcow2/disk.qcow2" "$disk"

# The update: the same image and one more layer, a different deployment, carried
# to the guest on a small disk of its own (there is no sshd to copy it with).
say 'build the update the machine will stage'
mkdir -p "$work/update"
printf 'FROM localhost/tilo-host:ci\nRUN echo %s > /usr/lib/tilo/host-assurance-update\n' "$target" \
  > "$work/update/Containerfile"
podman build -q -t "$update_ref" -f "$work/update/Containerfile" "$work/update" >/dev/null
podman save --format oci-archive -o "$work/update.tar" "$update_ref"
rm -f "$work/update.raw"
# The disk holds the whole archive the machine stages, so size it from the
# archive: a fixed number here would fail as a full filesystem on whichever
# image grew past it, and name nothing that said why.
archive_mib=$(( ($(stat -c %s "$work/update.tar") + 1048575) / 1048576 ))
truncate -s "$(( archive_mib + 256 ))M" "$work/update.raw"
mkfs.ext4 -q -F -L tiloupdate "$work/update.raw"
mkdir -p "$work/mnt"
mount -o loop "$work/update.raw" "$work/mnt"
cp "$work/update.tar" "$work/mnt/update.oci.tar"
umount "$work/mnt"
rm -f "$work/update.tar"
qemu-img convert -f raw -O qcow2 "$work/update.raw" "$work/update.qcow2"
rm -f "$work/update.raw"
podman rmi -f "$update_ref" "$image" >/dev/null 2>&1 || true

export TILO_DISTROBOX="" TILO_ACCEL=kvm TILO_USER TILO_PASS
export TILO_RUN=$work/run TILO_DISK=$disk
export TILO_SERIAL_SOCKET=$work/run/serial.sock
export TILO_CONSOLE_LOG=$out/console.log TILO_CONSOLE_USER=$TILO_USER TILO_CONSOLE_PASS=$TILO_PASS
mkdir -p "$TILO_RUN"
boot_vm() {
  ./os/dev/vm.sh boot --2d --disk "$disk" --attach "$work/update.qcow2"
}
finish() {
  ./os/dev/vm.sh serial > "$out/serial.log" 2>&1 || true
  ./os/dev/vm.sh stop >/dev/null 2>&1 || true
}
trap finish EXIT
console() { ./os/tests/serial_runner.sh "$1"; }
as_root() { console "printf '%s\n' '$TILO_PASS' | sudo -S -p '' bash -c $(printf '%q' "$1")"; }
ok()   { printf 'ok    %s\n' "$*" >&2; }
refuse() { local what="$1"; shift; as_root "$@" > "$out/$what.txt" 2>&1 || die "FAIL $what (see $out/$what.txt)"; ok "$what"; }

boot_vm
for _ in $(seq 1 60); do console 'true' >/dev/null 2>&1 && break; sleep 2; done
console 'true' >/dev/null || die 'the installed disk never answered at its console'

say 'the machine serves'
# What is up: multi-user reached, the hosted-VM service bound its socket, the API
# server inert until configured, the update timers enabled, the boot path judging
# itself by the server probe.
refuse 'multi-user.target is active' 'systemctl is-active --quiet multi-user.target'
refuse 'the API server is enabled and inert without its configuration' \
  'systemctl is-enabled --quiet tilo-api.service && ! systemctl is-active --quiet tilo-api.service'
refuse 'the update timers are enabled' \
  'systemctl is-enabled --quiet bootc-fetch-apply-updates.timer tilo-server-update-apply.timer'
refuse 'updates are staged through the verified path, never applied blindly' \
  'systemctl cat bootc-fetch-apply-updates.service | grep -qx "ExecStart=/usr/libexec/tilo-verified-update --quiet"'
refuse 'Firecracker is the pinned release' \
  'test "$(/usr/local/bin/firecracker --version | head -1)" = "Firecracker v1.17.0"'
# The VM host fetches the pinned guest from the registry before it serves, which
# a cold check pays for once; the socket is its own proof that it did.
for _ in $(seq 1 120); do
  as_root 'test -S /run/tilo-hosted/hosted.sock' && break
  sleep 5
done
as_root 'test -S /run/tilo-hosted/hosted.sock' >/dev/null \
  || { as_root 'journalctl -b --no-pager -u tilo-hosted | tail -40' >&2; die 'tilo-hosted never bound /run/tilo-hosted/hosted.sock'; }
ok 'the hosted-VM service is serving its socket' 

# The boot's own verdict, through the production unit: a machine not paired yet
# is judged by reaching multi-user, and that verdict is what boot-complete asks.
as_root 'systemctl start tilo-server-health.service' >/dev/null 2>&1 \
  || die 'tilo-server-health says this boot is not good (see the journal below)'
as_root 'journalctl -b --no-pager -u tilo-server-health' > "$out/server-health.txt" 2>&1 || true
grep -q 'not paired yet' "$out/server-health.txt" \
  || die "tilo-server-health did not judge the pairing state it booted in; see $out/server-health.txt"
ok 'the boot judged itself: not paired yet, multi-user reached' 
as_root 'systemctl start tilo-boot-health.service' >/dev/null 2>&1 \
  || die 'tilo-boot-health does not accept the server probe as this machine'"'"'s verdict'
ok 'tilo-boot-health judges the server by tilo-server-health' 

say 'stage the update and break it on the staged deployment only'
as_root 'mkdir -p /mnt/update && mount /dev/vdb /mnt/update && skopeo copy --quiet \
  oci-archive:/mnt/update/update.oci.tar oci:/mnt/update/image:tilo-host' > "$out/stage-transfer.txt" 2>&1 \
  || die "could not put the update image on the machine; see $out/stage-transfer.txt"
ok 'the update image is on the machine' 

RUNNER='./os/tests/serial_runner.sh' SUDO_PASS="$TILO_PASS" FAULT=server \
  SUBJECT_UNIT=tilo-server-health.service \
  RESET="./os/dev/vm.sh stop; ./os/dev/vm.sh boot --2d --disk '$disk' --attach '$work/update.qcow2'" \
  SERIAL='./os/dev/vm.sh serial' \
  BOOT_TIMEOUT=900 HEALTH_TIMEOUT=900 SWITCH_TIMEOUT=1800 \
  ./os/tests/boot_assessment_rollback.sh oci:/mnt/update/image:tilo-host "$out/boot-assessment"

say 'the machine serves again, on the deployment that came back'
refuse 'multi-user.target is active' 'systemctl is-active --quiet multi-user.target'
refuse 'the server probe still judges it good' 'systemctl start tilo-server-health.service'
printf '\nPASS: the server image boots serving, and an update that breaks it rolled back by itself (evidence in %s)\n' "$out"
