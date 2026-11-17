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

# Every step and every failure also goes to the console: a command that exits
# at once takes the guest's output port down with it, and what it wrote in its
# last instant never reached the log. The console is the stream the log keeps.
say() {
  printf '\n=== %s\n' "$*" >&2
  printf 'host-assurance: %s\n' "$*" >/dev/console 2>/dev/null || true
}
die() {
  printf 'host-assurance: %s\n' "$*" >&2
  printf 'host-assurance: FAIL %s\n' "$*" >/dev/console 2>/dev/null || true
  exit 1
}
# An exit at once takes the guest's output port with it, so a step that dies
# without a message of its own leaves none. Report the code on the console too.
report_exit() {
  local code=$?
  [ "$code" = 0 ] || printf 'assurance check: exited %s\n' "$code" >/dev/console 2>/dev/null || true
}
trap report_exit EXIT

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
# What this step is given and what it has to put it in, so a failure here says
# which of the two ran out rather than only that the step died.
printf 'image archive: %s bytes\n' "$(stat -c %s "$pub/image.oci.tar")" >&2
df -h /var /tmp /target 2>&1 >&2 || true
free -m >&2 || true
# A command whose failure must be readable: its stderr goes to the console as
# well, which the log keeps even when the step exits at once.
loud() {
  local err=$work/stderr code
  "$@" 2>"$err" || { code=$?; cat "$err" >/dev/console 2>/dev/null || true; return "$code"; }
  cat "$err" >&2 2>/dev/null || true
}
# Copying from oci-archive: unpacks the whole archive into TMPDIR first; keep
# that on the same disk as the rest of this step rather than wherever the
# sandbox last pointed TMPDIR.
export TMPDIR=$work/tmp
loud mkdir -p "$TMPDIR" || die "cannot make $TMPDIR"
# The image as an unpacked OCI layout, the way the desktop disk checks load
# theirs: a transport that reads the archive file itself holds every layer it
# unpacks in the step's memory, which is what ended this step on its 1.16 GB
# before it installed anything.
layout=$work/image.oci
loud rm -rf "$layout" || die "cannot clear $layout"
loud mkdir -p "$layout" || die "cannot make $layout"
loud tar -xf "$pub/image.oci.tar" -C "$layout" || die "cannot unpack $pub/image.oci.tar"
# The tag the build wrote into the archive's index; without one podman takes
# the layout's single image.
tag=$(loud python3 -c 'import json, sys
for entry in json.load(open(sys.argv[1])).get("manifests", []):
    name = (entry.get("annotations") or {}).get("org.opencontainers.image.ref.name", "").rsplit("/", 1)[-1]
    if ":" in name:
        print(name.rsplit(":", 1)[1])
        break' "$layout/index.json") || die "cannot read the tag in $layout/index.json"
# The archive names its image, and the build wrote that name beside it. Read it
# from the layout, not from podman: an image loaded out of a layout this way
# carries no repository digest, so podman's is empty and comparing it proves
# nothing.
digest=$(loud skopeo inspect --format '{{.Digest}}' "oci:$layout${tag:+:$tag}") \
  || die "skopeo cannot read the image in $pub/image.oci.tar"
[[ $digest == "$(cat "$pub/digest")" ]] \
  || die "$pub/image.oci.tar holds $digest, not the image $pub/digest names"
image=$(loud podman pull -q "oci:$layout${tag:+:$tag}") \
  || die "podman could not load the image in $pub/image.oci.tar"
loud podman tag "$image" localhost/tilo-host:ci || die "cannot name the loaded image"
say 'install the image on a disk'
TILO_IMAGE=localhost/tilo-host:ci TILO_SKIP_BUILD=1 TILO_DISTROBOX="" \
  TILO_OUTPUT=$work/install TILO_RUN=$work/install-run TILO_DISK_SIZE=64G \
  TILO_TARGET_IMGREF="registry.tilo.so/tilo/tilo-host:main-${target:0:7}-ci" \
  ./os/dev/build-image.sh 2> >(tee /dev/console >&2) \
  || die "the disk install failed"
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
# The disk holds what the machine stages: the whole archive, and the layout
# skopeo unpacks it into beside the archive. Size it from the archive rather
# than a fixed number, which fails as a full filesystem on whichever image grew
# past it and names nothing that said why.
archive_mib=$(( ($(stat -c %s "$work/update.tar") + 1048575) / 1048576 ))
truncate -s "$(( 2 * archive_mib + 256 ))M" "$work/update.raw"
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
# One line may import a gigabyte into the machine's storage (bootc switch) or
# check on a service that is still fetching its guest; the script's own waits
# are the bound on those, not the console runner's default for one command.
export TILO_CONSOLE_DEADLINE=1800
mkdir -p "$TILO_RUN"
boot_vm() {
  ./os/dev/vm.sh boot --2d --disk "$disk" --attach "$work/update.qcow2"
}
finish() {
  report_exit
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
  || { cat "$out/stage-transfer.txt" >/dev/console 2>/dev/null || true
       die "could not put the update image on the machine; see $out/stage-transfer.txt"; }
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
