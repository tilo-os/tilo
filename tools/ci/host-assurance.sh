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
# The installed root is LUKS2: the install opens it through device-mapper.
# The micro-VM runs no udev, so loading the module leaves /dev/mapper/control
# absent — the same reason the install materializes the loop partitions' nodes
# itself.
modprobe dm_mod 2>/dev/null || true
install -d -m 0755 /dev/mapper
[ -c /dev/mapper/control ] || mknod /dev/mapper/control c 10 236 2>/dev/null || true
test -c /dev/mapper/control \
  || die 'no device-mapper, which the encrypted install opens the volume with'

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
say 'install the image on an encrypted disk'
TILO_IMAGE=localhost/tilo-host:ci TILO_SKIP_BUILD=1 TILO_DISTROBOX="" \
  TILO_OUTPUT=$work/install TILO_RUN=$work/install-run TILO_DISK_SIZE=64G \
  TILO_ENCRYPT_INSTALL=1 \
  TILO_TARGET_IMGREF="registry.tilo.so/tilo/tilo-host:main-${target:0:7}-ci" \
  ./os/dev/build-image.sh 2> >(tee /dev/console >&2) \
  || die "the disk install failed"
disk=$work/disk.qcow2
mv "$work/install/qcow2/disk.qcow2" "$disk"
# The install made one recovery key and handed it over in a file, never in the
# log. It is this check's way into the machine at every boot: the disk opens
# with nothing typed only where the image carries a signed boot chain for a TPM
# keyslot, and this image does not (it boots split entries, not a sealed UKI).
[[ -s $work/install/recovery-key.txt ]] \
  || die "the encrypted install left no recovery key; the disk would be unopenable"
TILO_UNLOCK_SECRET=$(cat "$work/install/recovery-key.txt")
[[ -n $TILO_UNLOCK_SECRET ]] || die "the install's recovery key file is empty"
export TILO_UNLOCK_SECRET

# The update: the same image and one more layer, a different deployment, carried
# to the guest on a small disk of its own (there is no sshd to copy it with).
say 'build the update the machine will stage'
mkdir -p "$work/update"
{
  printf 'FROM localhost/tilo-host:ci\n'
  printf 'RUN echo %s > /usr/lib/tilo/host-assurance-update\n' "$target"
  # The update whose server cannot serve: the probe the boot judges itself by
  # fails on this image and nowhere else. It is part of the image rather than
  # a drop-in written at run time because a staged deployment's /etc is made
  # when the deployment is, and the check writes after that.
  printf '%s\n' \
    'RUN install -d -m 0755 /usr/lib/systemd/system/tilo-server-health.service.d \' \
    " && printf '[Service]\\nExecStartPre=/usr/bin/false\\n' \\" \
    '      > /usr/lib/systemd/system/tilo-server-health.service.d/90-boot-assessment-fault.conf'
} > "$work/update/Containerfile"
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

# The prompt comes before any network, so the serial console is the only input,
# and the console runner cannot be used to ask whether the guest is up: while
# the volume is closed it waits out its own deadline. The log the unlock helper
# keeps is the signal instead: a login prompt in it means the volume opened.
unlock_until_console() {
  local tries="${1:-120}"
  for _ in $(seq 1 "$tries"); do
    ./os/dev/unlock-prompt.sh "$TILO_UNLOCK_SECRET" >/dev/null 2>&1 || true
    if tail -c 20000 "$TILO_CONSOLE_LOG" 2>/dev/null | grep -qi 'login:'; then
      console 'true' >/dev/null 2>&1 && return 0
    fi
    sleep 3
  done
  return 1
}
boot_vm
unlock_until_console \
  || die 'the installed disk never answered at its console: its encrypted root did not open'
console 'true' >/dev/null || die 'the installed disk never answered at its console'
# Keep the console to the runner: kernel messages — auditd's lines among them,
# one per sudo call — interleave mid-line with the marker it reads, and a marker
# split that way waits out the command's whole deadline. An unpaired server also
# prints its pairing prompt here on every attempt, and a line that lands inside
# a read is read as part of it: this machine is never paired by this check.
as_root 'dmesg -n 1; systemctl stop tilo-server-firstboot.service' >/dev/null 2>&1 || true

say 'the machine serves'
# What is up: multi-user reached, the hosted-VM service bound its socket, the API
# server inert until configured, the update timers enabled, the boot path judging
# itself by the server probe.
# A console answers at its getty before the boot has settled: firstboot starts
# before multi-user.target (for up to its own enrollment timeout), so the target
# is not reached just because someone can type. Wait for it as a person would.
for _ in $(seq 1 90); do
  as_root 'systemctl is-active --quiet multi-user.target' && break
  sleep 5
done
refuse 'multi-user.target is active' 'systemctl is-active --quiet multi-user.target'
say 'the installed machine keeps its root on an encrypted disk'
refuse 'the root is a LUKS2 volume the initramfs opened' '
set -eu
src=$(findmnt -nro SOURCE /sysroot 2>/dev/null || findmnt -nro SOURCE /)
case "$src" in
  /dev/mapper/*) ;;
  *) echo "root is $src, not a device-mapper volume"; exit 1 ;;
esac
name=${src#/dev/mapper/}
cryptsetup status "$name" | grep -q LUKS2 || { cryptsetup status "$name"; exit 1; }
backing=""
while read -r n t; do
  [ "$t" = part ] || continue
  backing=$n
  break
done <<EOF
$(lsblk -sno NAME,TYPE "$src")
EOF
[ -n "$backing" ] || { echo "no partition under $src"; lsblk -s "$src"; exit 1; }
cryptsetup isLuks "/dev/$backing" || { echo "/dev/$backing is not LUKS"; exit 1; }
echo "the root is $src: LUKS2 over /dev/$backing"
'
refuse 'the root has the recovery key and no throwaway install key' '
set -eu
src=$(findmnt -nro SOURCE /sysroot)
name=${src#/dev/mapper/}
dump=$(cryptsetup luksDump "$name")
printf "%s\n" "$dump" | grep -q systemd-recovery \
  || { echo "no systemd-recovery token in the header"; exit 1; }
slots=$(printf "%s\n" "$dump" | sed -n "/^Keyslots:/,/^Tokens:/p" | grep -cE "^  [0-9]+:")
[ "$slots" = 1 ] \
  || { echo "$slots keyslots; the throwaway install key was not removed"; exit 1; }
echo "one keyslot: the recovery key the install handed over"
'
refuse 'the store volume is LUKS2 and mounts' '
set -eu
cryptsetup status tilo-store | grep -q LUKS2
findmnt -nro FSTYPE /var/lib/tilo/store | grep -qx btrfs
grep -q tilo-store /etc/crypttab
echo "the store is a LUKS2 + btrfs volume, unlocked from the root"
'
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
# The wrapper a server judges itself with, run in a unit of its own with the
# environment the image's drop-in gives the real one. The real unit's own state
# is the verdict the boot loader's boot is read by downstream: an uncounted
# boot's is "not assessed", and the rollback check takes anything else for a
# verdict the boot gave. So this check must not start it — only prove the same
# wrapper, with the same configuration, accepts this machine's server probe.
as_root 'systemctl reset-failed host-assurance-boot-health.service' >/dev/null 2>&1 || true
# The environment is read inside the guest: the console carries other writers'
# lines too (a pairing prompt among them), and a value that makes a round trip
# through it comes back with them in it.
as_root 'systemd-run --quiet --wait --unit=host-assurance-boot-health --service-type=oneshot \
  --property="Environment=$(systemctl show -p Environment --value tilo-boot-health.service)" \
  /usr/libexec/tilo-boot-health' \
  || { as_root 'journalctl -b --no-pager -u host-assurance-boot-health' > "$out/boot-health.txt" 2>&1 || true
       cat "$out/boot-health.txt" >/dev/console 2>/dev/null || true
       die 'tilo-boot-health does not accept the server probe as this machine'"'"'s verdict'; }
as_root 'journalctl -b --no-pager -u host-assurance-boot-health' > "$out/boot-health.txt" 2>&1 || true
grep -q 'tilo-server-health.service is active' "$out/boot-health.txt" \
  || die "tilo-boot-health did not judge the server by its probe; see $out/boot-health.txt"
ok 'tilo-boot-health judges the server by tilo-server-health'
as_root 'systemctl reset-failed host-assurance-boot-health.service' >/dev/null 2>&1 || true

say 'stage the update and break it on the staged deployment only'
as_root 'mkdir -p /mnt/update && mount /dev/vdb /mnt/update && skopeo copy --quiet \
  oci-archive:/mnt/update/update.oci.tar oci:/mnt/update/image:tilo-host' > "$out/stage-transfer.txt" 2>&1 \
  || { cat "$out/stage-transfer.txt" >/dev/console 2>/dev/null || true
       die "could not put the update image on the machine; see $out/stage-transfer.txt"; }
ok 'the update image is on the machine'

# A failed check keeps no evidence files, and the boot it died on is the only
# evidence there is: say what that boot recorded of its own verdict, its probe
# and any ordering cycle the boot loader resolved against them.
if ! RUNNER='./os/tests/serial_runner.sh' SUDO_PASS="$TILO_PASS" FAULT=server \
     SUBJECT_UNIT=tilo-server-health.service \
     UNLOCK='./os/dev/unlock-prompt.sh' \
     RESET="./os/dev/vm.sh stop; ./os/dev/vm.sh boot --2d --disk '$disk' --attach '$work/update.qcow2'; ./os/dev/unlock-prompt.sh || true" \
     SERIAL='./os/dev/vm.sh serial' \
     BOOT_TIMEOUT=900 HEALTH_TIMEOUT=900 SWITCH_TIMEOUT=1800 \
     ./os/tests/boot_assessment_rollback.sh oci:/mnt/update/image:tilo-host "$out/boot-assessment"; then
  as_root 'echo "--- health"; systemctl show -p ActiveState -p Result -p ConditionResult tilo-boot-health.service
           echo "--- probe"; systemctl show -p ActiveState -p Result tilo-server-health.service
           echo "--- cycles"; journalctl -b --no-pager 2>/dev/null | grep -i "ordering cycle\|deleted to break" | head -20
           echo "--- health journal"; journalctl -b --no-pager -u tilo-boot-health -u systemd-bless-boot -u tilo-server-health 2>/dev/null | tail -40' \
    || true
  exit 1
fi

say 'the machine serves again, on the deployment that came back'
refuse 'multi-user.target is active' 'systemctl is-active --quiet multi-user.target'
refuse 'the server probe still judges it good' 'systemctl start tilo-server-health.service'
printf '\nPASS: the server image boots serving, and an update that breaks it rolled back by itself (evidence in %s)\n' "$out"
