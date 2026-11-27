#!/usr/bin/env bash
# The server image proves itself by booting, in a VM: the machine boots the boot
# loader and UKI a release signs under Secure Boot and refuses a UKI whose command
# line was edited or that nobody signed, comes up serving (the hosted-VM service
# binds its socket, the boot judges itself, an update stages through the verified
# path), an update that breaks the server rolls itself back without anyone at the
# machine, and an update from a registry is verified before it stages.
#
# The image is the one image-host built, its boot chain signed as a release signs
# it (sign-boot-loader, os-release.sh sign-boot, host-seal.sh apply) with
# throwaway EFI and PCR keys, one check-only layer more so the image trusts this
# check's PCR key the way a release trusts the release one, installed the way the
# desktop's dev disk is (os/dev/build-image.sh, bootc install to-filesystem) onto
# a LUKS2 root with a LUKS2 store, the layout the installer makes
# (TILO_ENCRYPT_INSTALL=1). That install opens the volume through device-mapper,
# which this check's micro-VM has not, so it runs in a guest of its own, booted by
# the same VM harness with a software TPM; the machine under test then boots that
# disk with the same TPM, under firmware that enforces Secure Boot with the
# throwaway EFI certificate as its only key, and the recovery key the install
# handed over is what opens it the first time.
#
# The installer enrolled its TPM keyslot in a guest whose PCR 7 is not the
# machine's, so it cannot open there. The installed machine seals the volume to
# itself — the shipped tilo-rollback enrol, against the signed PCR 11 prediction
# and this machine's live PCR 7 — and reboots with nothing typed to prove the
# PIN-less keyslot opens it. Then the floor moves: a signed approval per increment,
# requested by the machine and issued by the throwaway authority, and an older
# signed image (its measurement authorized as generation 0, below any floor) must
# not unseal. The update staged on it is the same image with one more layer, sealed
# and signed the same way, so bootc sees a different deployment the firmware still
# accepts; os/tests/boot_assessment_rollback.sh then stages it, breaks only it
# and reboots until the boot loader has spent its tries and gone back, answering
# each boot's unlock prompt. The server image runs no sshd, so every step is
# driven at the serial console, which is also how a person reaches it
# (os/tests/serial_runner.sh).
#
# The record of every boot lands in /target/host-assurance.
#
#   host-assurance.sh                      the whole proof (host-assurance-x86_64)
#   host-assurance.sh base OUT             everything before the stages, and the first boot,
#                                          kept in OUT (publish-host-assurance-base)
#   host-assurance.sh fetch OUT            the base tools/ci/host-assurance-base.env names,
#                                          unpacked (host-assurance-base-fetch)
#   host-assurance.sh stage BASE STAGE...  those stages, on a copy of the fetched BASE
#                                          (host-assurance-stage-*)
#
# The stages, in the order the whole proof runs them: serves, refused, rollback, registry,
# seal, floor (floor runs on a sealed disk, so after seal). The base is the expensive part:
# the signed and sealed images, the install onto LUKS2, the update disk and the firmware's
# keys, and a first boot that serves. A stage boots an overlay of the base's disk with a copy
# of its TPM and firmware variables, so a fix to this script or to a stage's own checks is
# proved again in one boot of minutes, not the hours the images take. A change to a program
# on the image needs a new base: the image is sealed, and nothing merges over it at boot.
#
# OUT/timings.tsv records each step's seconds; the log ends with them as a table.
set -euo pipefail
mode=${1:-full}
case $mode in
  full) ;;
  base | fetch) base_out=${2:?usage: host-assurance.sh $mode OUT} ;;
  stage)
    base_in=${2:?usage: host-assurance.sh stage BASE STAGE...}
    shift 2
    stages=("$@")
    (( ${#stages[@]} > 0 )) || { echo 'host-assurance: name the stages to run' >&2; exit 2; }
    ;;
  *) echo "host-assurance: unknown mode $mode" >&2; exit 2 ;;
esac
if [[ $mode == fetch ]]; then
  # Outside the stages' VM, whose user-mode network is slow; t keeps it under the base file's
  # key, so the stages on one base fetch it once.
  env_file=tools/ci/host-assurance-base.env
  [[ -s $env_file ]] || { echo "host-assurance: no base: dispatch publish-host-assurance-base on main (t dispatch --repo tilo --ref main publish-host-assurance-base --publish) and commit the COMMIT= and IMAGE= lines its log ends with as $env_file" >&2; exit 1; }
  base_commit=$(sed -n 's/^COMMIT=//p' "$env_file") base_image=$(sed -n 's/^IMAGE=//p' "$env_file")
  [[ $base_commit =~ ^[0-9a-f]{40}$ && $base_image =~ ^registry\.tilo\.so/[a-z0-9/_-]+@sha256:[0-9a-f]{64}$ ]] \
    || { echo "host-assurance: $env_file names no commit and image by digest" >&2; exit 1; }
  rm -rf "$base_out" && mkdir -p "$base_out"
  python3 tools/ci/publish/server-plan-base.py fetch "$base_image" "$base_out/base" > "$base_out/base-config.json"
  [[ $(jq -r .commit "$base_out/base-config.json") == "$base_commit" ]] \
    || { echo "host-assurance: the base image was built from $(jq -r .commit "$base_out/base-config.json"), not $base_commit" >&2; exit 1; }
  du -sh --apparent-size "$base_out/base"
  exit 0
fi
. tools/ci/publish/lib.sh
# The scratch registry the update below is served from.
# shellcheck source=os/scratch-registry.env
. "$repo_root/os/scratch-registry.env"

arch=x86_64
work=/var/tmp/host-assurance
out=${CARGO_TARGET_DIR:-/target}/host-assurance
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
  local code=${1:-$?}
  [ "$code" = 0 ] || printf 'assurance check: exited %s\n' "$code" >/dev/console 2>/dev/null || true
}
trap report_exit EXIT
step_name="" step_started=$SECONDS
step() {
  [[ -z $step_name ]] || printf '%s\t%s\n' "$step_name" "$((SECONDS - step_started))" >> "$out/timings.tsv"
  step_name=$1 step_started=$SECONDS
}
table() {
  step ""
  [[ -s $out/timings.tsv ]] || return 0
  { printf '\n%-28s %8s\n' step seconds
    awk -F'\t' '{ printf "%-28s %8s\n", $1, $2; total += $2 } END { printf "%-28s %8s\n", "total", total }' "$out/timings.tsv"
  } >&2
}

bash tools/ci/install-packages.sh tools/ci/packages/boot-assessment.txt
rpm -q binutils openssl >/dev/null 2>&1 \
  || dnf install -y --setopt=install_weak_deps=False binutils openssl >/dev/null
command -v qemu-system-x86_64 qemu-img podman skopeo systemd-repart >/dev/null
./os/dev/assert-native-arch.sh "$arch"
python3 os/dev/probe-kvm.py "$arch" || {
  echo 'this node cannot create a KVM VM; boots under TCG are too slow to count' >&2
  exit 1
}

rm -rf "$work" "$out"
mkdir -p "$work" "$out"
: > "$out/timings.tsv"
# Before the guest: the image signed, sealed and installed, the firmware's keys, the update
# disk and what the registry serves. Everything a stage needs after it is in $work (the
# disk, the update disk, the firmware variables, the TPM, the boot keys, the recovery key,
# $work/registry, $work/base.vars), which is what the base keeps.
prepare() {
step build
target=$(git rev-parse HEAD)
built=/target/publish/host
[[ -s $built/image.oci.tar && -s $built/digest && -s $built/generation ]] \
  || die "no server image in $built: the check needs image-host"

modprobe fuse 2>/dev/null || true
modprobe loop 2>/dev/null || true
test -c /dev/fuse || die 'no /dev/fuse, which the install needs'
test -c /dev/loop-control || die 'no loop devices, which the install needs'
losetup --find >/dev/null

say 'install the server image'
# What this step is given and what it has to put it in, so a failure here says
# which of the two ran out rather than only that the step died.
printf 'server image archive: %s bytes\n' "$(stat -c %s "$built/image.oci.tar")" >&2
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
scratch=$TMPDIR

# The boot chain signed as a release signs it (sign-boot-loader, os-release.sh sign-boot,
# host-seal.sh apply), with throwaway EFI and PCR keys made here: the firmware below
# trusts those keys and nothing else, and the installed image trusts the throwaway PCR
# key the way a released image trusts the release one.
export TILO_BOOT_SIGNER_DRY_RUN=$work/boot-keys TILO_BOOT_SIGNING_DRY_RUN=1
bash tools/signing/boot-signer.sh export-efi-cert "$work/efi.crt" \
  || die 'cannot make the throwaway boot signing keys'
signed_image() {  # SEALED OUT: SEALED's boot chain signed, the image into OUT
  bash tools/ci/publish/os-release.sh sign-boot "$1" "$work/boot-signed" 2> >(tee /dev/console >&2) \
    || die "could not sign the boot chain of $1"
  bash tools/ci/publish/host-seal.sh apply "$1" "$work/boot-signed" "$2" 2> >(tee /dev/console >&2) \
    || die "could not add the signed boot chain to $1"
  rm -rf "$work/boot-signed"
}
say 'build the image a release ships: its boot loader signed, its PCR authority this machine'
# The image image-host built carries the release PCR verification key, and the
# initramfs that verifies a prediction was built with it: it is inside the UKI,
# so a keyslot sealed to a prediction this check signs could never open. So this
# check's image, before the seal computes its digest, gets the throwaway public
# key in place of the release one (rootfs and initramfs alike) and the boot
# loader signed with the throwaway EFI key (tools/ci/publish/sign-boot-loader.sh,
# the release's own step). Everything after is the release path unchanged.
built_name=localhost/tilo-host:built
load_archive "$built" "$built_name"
mkdir -p "$work/uki-dir" "$work/kernel"
podman create --name host-assurance-built "$built_name" >/dev/null
podman cp "host-assurance-built:/boot/EFI/Linux/." "$work/uki-dir"
podman cp "host-assurance-built:/kernel/." "$work/kernel"
podman rm host-assurance-built >/dev/null
kver=$(ls "$work/kernel")
[[ -s $work/kernel/$kver/vmlinuz ]] || die "the image carries no /kernel/$kver/vmlinuz"
set -- "$work"/uki-dir/*.efi
[[ $# -eq 1 ]] || die 'the image does not carry exactly one UKI'
uki_name=$(basename "$1")
objcopy --dump-section .cmdline="$work/uki.cmdline" "$1"
objcopy --dump-section .osrel="$work/uki.osrel" "$1"
tr -d '\000' < "$work/uki.cmdline" > "$work/cmdline"
tr -d '\000' < "$work/uki.osrel" > "$work/osrel"
mkdir -p "$work/trusted"
printf 'FROM %s\nCOPY pcr.pem /usr/share/tilo/tpm2-pcr-signing.public.pem\n' "$built_name" \
  > "$work/trusted/Containerfile"
cp "$work/boot-keys/pcr.pem" "$work/trusted/pcr.pem"
SOURCE_DATE_EPOCH=$(podman image inspect --format '{{.Created.Unix}}' "$built_name") \
  podman build -q --pull=never -t localhost/tilo-host:trusted \
    -f "$work/trusted/Containerfile" "$work/trusted" >/dev/null \
  || die "cannot build the image that trusts this check's PCR authority"
# The initramfs is the image's own recipe (os/scripts/build-initramfs.sh, the
# headless module set), run over the image that now holds this check's key.
podman run --rm --privileged --pull=never --network=none -v "$work:/work:Z" \
  --entrypoint bash localhost/tilo-host:trusted -c '
set -euo pipefail
TILO_INITRAMFS_MODULES="crypt systemd-cryptsetup tpm2-tss tilo-rollback" \
  bash /usr/local/bin/build-initramfs.sh "$1"
cp "/usr/lib/modules/$1/initramfs.img" /work/initramfs.img
' initramfs "$kver" 2> >(tee /dev/console >&2) \
  || die 'cannot build the initramfs that trusts this check'"'"'s PCR key'
# And the UKI around it: the image's kernel, command line and os-release, which
# is the same assembly the image build runs (os/dev/uki.sh).
mkdir -p "$work/uki-tools"
tar -xf /target/publish/host-base/uki-tools/image.oci.tar -C "$work/uki-tools"
podman tag "$(podman pull -q "oci:$work/uki-tools")" localhost/tilo-uki-tools:ci
podman run --rm --pull=never --network=none -v "$work:/work:Z" \
  --entrypoint ukify localhost/tilo-uki-tools:ci build \
  --linux "/work/kernel/$kver/vmlinuz" --initrd /work/initramfs.img --uname "$kver" \
  --cmdline "@/work/cmdline" --os-release "@/work/osrel" --output /work/uki.efi \
  2> >(tee /dev/console >&2) || die 'cannot assemble the image'"'"'s UKI with this check'"'"'s initramfs'
printf 'FROM localhost/tilo-host:trusted\nCOPY uki.efi /boot/EFI/Linux/%s\n' "$uki_name" \
  > "$work/trusted/Containerfile"
cp "$work/uki.efi" "$work/trusted/uki.efi"
SOURCE_DATE_EPOCH=$(podman image inspect --format '{{.Created.Unix}}' localhost/tilo-host:trusted) \
  podman build -q --pull=never -t localhost/tilo-host:trusted-image \
    -f "$work/trusted/Containerfile" "$work/trusted" >/dev/null \
  || die 'cannot put the rebuilt UKI into the image'
mkdir -p "$work/trusted-image"
save_archive localhost/tilo-host:trusted-image "$work/trusted-image"
cp "$built/rpms-source" "$work/trusted-image/rpms-source"
bash tools/ci/publish/sign-boot-loader.sh "$work/trusted-image" "$work/loader-signed" \
  2> >(tee /dev/console >&2) || die "cannot sign the image's boot loader"
bash tools/ci/publish/host-seal.sh seal "$work/loader-signed" "$work/sealed" \
  2> >(tee /dev/console >&2) || die 'cannot seal the signed image'
sealed=$work/sealed
say 'sign the sealed server image'
signed_image "$sealed" "$work/signed"
pub=$work/signed


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

# An older signed image, for the floor to refuse later: the same rootfs and one
# layer more, sealed and signed as the release signs (host-seal.sh seal,
# os-release.sh sign-boot), its measurement signed as generation 0 — the
# generation before the server's dense series starts. It is not installed; what
# it proves is that the TPM will not unseal a generation below the floor.
# Built over the sealed image, not the signed one: the seal reseals the image's
# UKIs in one layer of its own, and the signed image already carries a second
# UKI (/boot/tilo-target) that would make that two.
say 'build an older signed image'
older=localhost/tilo-host:older
n=$(cat "$pub/generation")
[[ $n =~ ^[1-9][0-9]*$ ]] || die "the sealed image carries no generation"
load_archive "$sealed" localhost/tilo-host:sealed
mkdir -p "$work/older"
printf 'FROM localhost/tilo-host:sealed\nRUN echo %s > /usr/lib/tilo/host-assurance-older\n' "$target" \
  > "$work/older/Containerfile"
SOURCE_DATE_EPOCH=$(podman image inspect --format '{{.Created.Unix}}' localhost/tilo-host:sealed) \
  podman build -q --pull=never -t "$older" -f "$work/older/Containerfile" "$work/older" >/dev/null \
  || die 'cannot build the older image'
mkdir -p "$work/older-built"
save_archive "$older" "$work/older-built"
cp "$pub/rpms-source" "$work/older-built/rpms-source"
bash tools/ci/publish/host-seal.sh seal "$work/older-built" "$work/older-sealed" \
  2> >(tee /dev/console >&2) || die 'cannot seal the older image'
bash tools/ci/publish/os-release.sh sign-boot "$work/older-sealed" "$work/older-signed" \
  2> >(tee /dev/console >&2) || die 'cannot sign the older image'
# The prediction the authority signs is over approved_policy || generation (the
# same message sign-target-prediction.sh prepare makes), so naming generation 0
# here and signing again is what makes this image's seal an older one.
printf '0\n' > "$work/older-signed/boot/generation"
python3 - "$work/older-signed/boot/approved.bin" "$work/older-signed/boot/message.bin" <<'PY'
import struct, sys
approved, message = sys.argv[1:]
open(message, 'wb').write(open(approved, 'rb').read() + struct.pack('>Q', 0))
PY
bash tools/signing/boot-signer.sh sign-sha256-rsa-pkcs1 \
  "$work/older-signed/boot/message.bin" "$work/older-signed/boot/message.sig" \
  || die "cannot sign the older image's prediction"
load_archive "$work/older-sealed" "$older"
TILO_BOOT_SIGNING_DRY_RUN=1 bash os/dev/sign-target-prediction.sh apply "$older" "$work/older-signed/boot" \
  2> >(tee /dev/console >&2) || die "cannot add the older image's signed boot files"
podman create --name host-assurance-older "$older" >/dev/null
mkdir -p "$work/older-efi" "$work/older-predictions"
podman cp "host-assurance-older:/boot/EFI/Linux/." "$work/older-efi"
podman cp "host-assurance-older:/boot/tilo-target/predictions/." "$work/older-predictions"
podman rm host-assurance-older >/dev/null
[[ $(printf '%s\n' "$work"/older-efi/*.efi | wc -l) -eq 1 ]] || die 'the older image does not carry exactly one UKI'
grep -q '"generation":0' "$work/older-predictions"/generation-0.json \
  || die 'the older image carries no generation-0 prediction'

# The install runs in a guest of the check's own, booted by the same VM harness
# the machine under test is: this check's micro-VM has no device-mapper (dm_mod
# does not load in it), and the encrypted layout opens the volume through one.
# The guest is this tree's dev disk — the machine the desktop's encrypted-install
# check installs from — and the command inside it is the one the check ran on its
# own disk before encryption: os/dev/build-image.sh with TILO_ENCRYPT_INSTALL=1,
# unchanged, so what is proved here is the shipped path and not a second one.
step install
say 'install the image on an encrypted disk, in a guest that can open one'
dev_disk=/target/publish/os-dev-disk/x86_64/disk/disk.qcow2
[[ -s $dev_disk ]] || die "no dev disk in /target/publish/os-dev-disk: the check needs os-dev-disk-lean"
# The dev disk is an output other checks read too: this guest writes to an
# overlay of it, never to it.
qemu-img create -q -f qcow2 -F qcow2 -b "$dev_disk" "$work/guest.qcow2"
imgref="registry.tilo.so/tilo/tilo-host:main-${target:0:7}-ci"
# The install's clock: the tree the guest installs from is sent without its Git,
# and a zero clock is one DNF rejects every RPM header on.
epoch=$(git log -1 --format=%ct)
guest_work=/var/tmp/host-assurance-install
# The install guest and the machine under test are two boots of one emulated
# TPM, which is the device a keyslot is sealed to: the TPM state lives beside
# both runs, and each boot has a run directory of its own so one machine's
# console log is never read as the other's.
export TILO_DISTROBOX="" TILO_ACCEL=kvm TILO_USER TILO_PASS
export TILO_TPM_DIR=$work/tpm
installer_run=$work/installer-run
TILO_RUN=$installer_run ./os/dev/vm.sh boot --2d --tpm --disk "$work/guest.qcow2" \
  || die 'the install guest did not boot'
# The guest is driven over the SSH a dev disk gives the VM harness, as root
# through sudo: the install runs with the privileges it would have had here.
unset SSH_ASKPASS SSH_ASKPASS_REQUIRE
ssh_guest() {
  sshpass -p "$TILO_PASS" ssh -F /dev/null -p "${TILO_SSH_PORT:-2222}" \
    -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o ConnectTimeout=8 -o ServerAliveInterval=10 -o ServerAliveCountMax=600 \
    -o LogLevel=ERROR -o NumberOfPasswordPrompts=1 \
    -o PreferredAuthentications=password -o PubkeyAuthentication=no \
    -o KbdInteractiveAuthentication=no "$TILO_USER@127.0.0.1" "$@"
}
as_guest_root() {
  ssh_guest "printf '%s\n' '$TILO_PASS' | sudo -S -p '' bash -c $(printf '%q' "$1")"
}
stop_installer() {
  TILO_RUN=$installer_run ./os/dev/vm.sh stop >/dev/null 2>&1 || true
}
trap 'stop_installer; report_exit' EXIT
for _ in $(seq 1 90); do ssh_guest true >/dev/null 2>&1 && break; sleep 10; done
ssh_guest true >/dev/null || { stop_installer; die 'the install guest never answered over ssh'; }
# What the guest installs from, sent in: this checkout's install tools (os/) and
# the image whose digest the check just proved, unpacked where the guest's own
# root podman loads it.
loud ssh_guest "install -d -m 0700 $guest_work/src $guest_work/image" \
  || die "could not make the install guest's work directories"
tar -C "$PWD" -cf - os | ssh_guest "tar -C $guest_work/src -xf -" \
  || die 'could not send the install tools to the guest'
tar -C "$layout" -cf - . | ssh_guest "tar -C $guest_work/image -xf -" \
  || die 'could not send the image to the guest'
cat > "$work/guest-install.sh" <<EOF
set -euo pipefail
# The container's own storage and scratch, made first: podman refuses a bind
# mount whose source it did not make itself.
install -d -m 0700 $guest_work/inner/containers $guest_work/inner/tmp
# The disk step runs in a container on the guest, as the installer medium's
# build does on its worker VM (tools/ci/medium-in-worker-vm.sh): the tools it
# needs are installed there, and the guest's own root is an immutable image that
# installs nothing. The container shares the guest's kernel, so the volume it
# opens is the guest's device-mapper.
podman run --rm --privileged --network host --security-opt label=disable \\
  -v /dev:/dev -v $guest_work:/var/w \\
  -v $guest_work/inner/containers:/var/lib/containers \\
  -v $guest_work/inner/tmp:/tmp \\
  -e HOME=/root -e TMPDIR=/tmp \\
  --entrypoint bash registry.fedoraproject.org/fedora:44 -eo pipefail -c '
set -euo pipefail
dnf install -y --setopt=install_weak_deps=False podman skopeo fuse-overlayfs e2fsprogs \\
  util-linux jq python3 dosfstools btrfs-progs systemd systemd-udev mtools qemu-img sudo curl \\
  tar gzip zstd cryptsetup shadow-utils >/dev/null
image=\$(podman pull -q oci:/var/w/image${tag:+:$tag})
podman tag "\$image" localhost/tilo-host:ci
cd /var/w/src
TILO_IMAGE=localhost/tilo-host:ci TILO_SKIP_BUILD=1 TILO_DISTROBOX= \\
TILO_ENCRYPT_INSTALL=1 TILO_DISK_SIZE=64G TILO_TARGET_IMGREF=$imgref \\
TILO_OUTPUT=/var/w/install TILO_RUN=/var/w/install-run \\
SOURCE_DATE_EPOCH=$epoch \\
  ./os/dev/build-image.sh
'
EOF
ssh_guest "cat > $guest_work/install.sh" < "$work/guest-install.sh" \
  || die 'could not put the install script on the guest'
installer_status=0
as_guest_root "bash $guest_work/install.sh" 2> >(tee /dev/console >&2) || installer_status=$?
if [[ $installer_status != 0 ]]; then
  # A failed install keeps its own log in its own guest: bring the reason out
  # while the guest is still up, then stop it.
  as_guest_root "tail -n 200 $guest_work/install/qcow2/.install.log 2>/dev/null" >&2 || true
  stop_installer
  die "the disk install failed in its guest (exit $installer_status)"
fi
# The disk and the one recovery key the install made, out of the guest while it
# is still up. The key is this check's way into the machine on its first boot;
# after that the machine seals the volume to itself.
mkdir -p "$work/install/qcow2"
# The disk is world-readable; the key is root's, so it comes out through sudo.
ssh_guest "tar -C $guest_work/install -cf - qcow2/disk.qcow2" \
  | tar -C "$work/install" -xf - \
  || { stop_installer; die 'could not take the installed disk out of the guest'; }
as_guest_root "cat $guest_work/install/qcow2/recovery-key.txt" \
  > "$work/install/qcow2/recovery-key.txt" \
  || { stop_installer; die "could not take the install's recovery key out of the guest"; }
chmod 0600 "$work/install/qcow2/recovery-key.txt"
stop_installer
disk=$work/disk.qcow2
mv "$work/install/qcow2/disk.qcow2" "$disk"
[[ -s $work/install/qcow2/recovery-key.txt ]] \
  || die "the encrypted install left no recovery key; the disk would be unopenable"
TILO_UNLOCK_SECRET=$(cat "$work/install/qcow2/recovery-key.txt")
[[ -n $TILO_UNLOCK_SECRET ]] || die "the install's recovery key file is empty"
export TILO_UNLOCK_SECRET

raw=$work/disk.raw
qemu-img convert -f qcow2 -O raw "$disk" "$raw"
# The install put the boot loader the image carries on the ESP: the `.efi.signed`
# companion sign-boot-loader.sh signed into it, both copies (bootctl selects it
# for EFI/systemd and EFI/BOOT). Nothing is signed here; the firmware's verdict
# below is about what the release path produced.
esp_offset=$(sfdisk -J "$raw" | python3 -c 'import json, sys
table = json.load(sys.stdin)["partitiontable"]
esp = [p for p in table["partitions"] if p["type"].upper() == "C12A7328-F81F-11D2-BA4B-00A0C93EC93B"]
assert len(esp) == 1, esp
print(esp[0]["start"] * table.get("sectorsize", 512), esp[0]["size"] * table.get("sectorsize", 512))') \
  || die "cannot find the ESP on $raw"
read -r esp_start esp_size <<<"$esp_offset"
mkdir -p "$work/esp"
mount -o "loop,offset=$esp_start,sizelimit=$esp_size" "$raw" "$work/esp" || die 'cannot mount the ESP'
podman create --name host-assurance-loader localhost/tilo-host:ci >/dev/null
podman cp host-assurance-loader:/usr/lib/systemd/boot/efi/systemd-bootx64.efi.signed "$work/loader.signed"
podman rm host-assurance-loader >/dev/null
[[ -s $work/loader.signed ]] || die 'the installed image carries no signed boot loader'
loaders=0
for loader in "$work"/esp/EFI/BOOT/BOOT*.EFI "$work"/esp/EFI/systemd/systemd-boot*.efi; do
  [[ -f $loader ]] || continue
  cmp -s "$work/loader.signed" "$loader" \
    || die "$loader is not the signed boot loader the image carries"
  loaders=$((loaders + 1))
done
ls -R "$work/esp" > "$out/esp.txt" 2>&1 || true
umount "$work/esp"
(( loaders > 0 )) || die 'the install put no systemd-boot on the ESP'
qemu-img convert -f raw -O qcow2 "$raw" "$disk"
rm -f "$raw"

# The firmware's variable store with Secure Boot on and the throwaway EFI
# certificate as its only key (os/dev/prepare-release-secureboot-vars.sh, as a
# release machine enrols the release certificate): anything that certificate did
# not sign is refused. The store's certificate check runs the image's own
# tilo-crypto, as the signing steps do.
rpm -q python3-virt-firmware >/dev/null 2>&1 \
  || dnf install -y --setopt=install_weak_deps=False python3-virt-firmware >/dev/null
mkdir -p "$work/bin"
cat > "$work/bin/tilo-crypto" <<'EOF'
#!/bin/sh
exec podman run --rm --network=none -v "$(dirname "$2"):/in:ro,Z" \
  --entrypoint /usr/libexec/tilo/tilo-crypto localhost/tilo-host:ci "$1" "/in/$(basename "$2")"
EOF
chmod 0755 "$work/bin/tilo-crypto"
bash tools/signing/boot-signer.sh export-efi-cert "$work/efi.crt"
efi_cert_sha256=$(openssl x509 -in "$work/efi.crt" -outform DER | sha256sum | cut -d' ' -f1)
vars_template=$(realpath -e "$(TILO_DISTROBOX="" TILO_RUN=$work/firmware ./os/dev/vm.sh firmware --tpm | awk '$1 == "vars" { print $2 }')")
[[ -f $vars_template ]] || die "no OVMF variable store template: '$vars_template'"
PATH=$work/bin:$PATH bash os/dev/prepare-release-secureboot-vars.sh "$vars_template" "$work/secureboot-vars.qcow2" \
  "$repo_root/tools/signing/boot-signer.sh" "$efi_cert_sha256" || die 'cannot enrol the throwaway EFI certificate'

# The update: the same image and one more layer, a different deployment, sealed
# and signed the way the image itself was (host-seal.sh seal, then the signing
# above), so it boots under the same firmware. It reaches the guest on a small
# disk of its own (there is no sshd to copy it with), beside the sealed image's
# UKI before anything signed it, which the firmware must refuse.
step update-disk
say 'build, seal and sign the update the machine will stage'
load_archive "$sealed" localhost/tilo-host:sealed
mkdir -p "$work/update"
{
  printf 'FROM localhost/tilo-host:sealed\n'
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
mkdir -p "$work/update-built"
save_archive "$update_ref" "$work/update-built"
cp "$sealed/rpms-source" "$work/update-built/rpms-source"
bash tools/ci/publish/host-seal.sh seal "$work/update-built" "$work/update-sealed" 2> >(tee /dev/console >&2) \
  || die 'could not seal the update'
signed_image "$work/update-sealed" "$work/update-signed"
rm -rf "$work/update-built" "$work/update-sealed"
# What the registry serves is a release of its own, sealed and signed the same way: the
# next image the server's channel publishes, not the update the rollback above stages and
# leaves behind. A registry never serves that one again as news.
registry_ref=localhost/tilo-host:assurance-registry
mkdir -p "$work/registry-built"
printf 'FROM localhost/tilo-host:sealed\nRUN echo %s > /usr/lib/tilo/host-assurance-registry\n' "$target" \
  > "$work/registry-built/Containerfile"
podman build -q -t "$registry_ref" -f "$work/registry-built/Containerfile" "$work/registry-built" >/dev/null \
  || die 'cannot build the image the registry serves'
rm -rf "$work/registry-built"
mkdir -p "$work/registry-built"
save_archive "$registry_ref" "$work/registry-built"
cp "$sealed/rpms-source" "$work/registry-built/rpms-source"
bash tools/ci/publish/host-seal.sh seal "$work/registry-built" "$work/registry-sealed" 2> >(tee /dev/console >&2) \
  || die 'could not seal the image the registry serves'
signed_image "$work/registry-sealed" "$work/registry-signed"
rm -rf "$work/registry-built" "$work/registry-sealed"
podman rmi -f "$registry_ref" >/dev/null 2>&1 || true
podman create --name host-assurance-unsigned localhost/tilo-host:sealed >/dev/null
podman cp host-assurance-unsigned:/boot/EFI/Linux/. "$work/unsigned"
podman rm host-assurance-unsigned >/dev/null
rm -f "$work/update.raw"
# The disk holds what the machine stages: the whole archive, and the layout
# skopeo unpacks it into beside the archive. Size it from the archive rather
# than a fixed number, which fails as a full filesystem on whichever image grew
# past it and names nothing that said why.
archive_mib=$(( ($(stat -c %s "$work/update-signed/image.oci.tar") + 1048575) / 1048576 ))
# And what the last steps need at the console: the recovery key that authorises
# sealing to this machine, the signed prediction of the generation it runs, and
# the older signed UKI with its signed prediction. The predictions come from the
# image, not from the machine's /boot: there the ESP is mounted over the image's
# own /boot, and this install put no copy of them on the ESP.
truncate -s "$(( 2 * archive_mib + 768 ))M" "$work/update.raw"
mkfs.ext4 -q -F -L tiloupdate "$work/update.raw"
mkdir -p "$work/mnt" "$work/original-efi"
mount -o loop "$work/update.raw" "$work/mnt"
cp "$work/update-signed/image.oci.tar" "$work/mnt/update.oci.tar"
cp "$work"/unsigned/*.efi "$work/mnt/unsigned.efi"
install -m 0600 "$work/install/qcow2/recovery-key.txt" "$work/mnt/recovery.key"
podman create --name host-assurance-original localhost/tilo-host:ci >/dev/null
podman cp "host-assurance-original:/boot/EFI/Linux/." "$work/original-efi"
mkdir -p "$work/mnt/predictions"
podman cp "host-assurance-original:/boot/tilo-target/predictions/." "$work/mnt/predictions"
podman rm host-assurance-original >/dev/null
compgen -G "$work/mnt/predictions/generation-$n.json" >/dev/null \
  || die "the installed image carries no signed prediction of generation $n"
cp "$work/original-efi"/*.efi "$work/mnt/original.efi"
cp "$work/older-efi"/*.efi "$work/mnt/older.efi"
mkdir -p "$work/mnt/older-predictions"
cp "$work/older-predictions"/* "$work/mnt/older-predictions/"

# The same update served by a registry on this machine, which the guest reaches
# as 10.0.2.2 and is told is a mirror of registry.tilo.so/tilo/tilo-host: under
# the tag the server was installed to follow, signed with a key made here for
# the repository's name, and beside it an image of the repository nobody signed
# (os/tests/registry_update_check.sh says what the guest does with them).
# The registry itself starts when the stage that reads it does (start_registry); what it
# serves is made here: the signed update and its signing key, kept in $work/registry.
mkdir -p "$work/registry"
printf 'host-assurance\n' > "$work/registry/sigstore.pass"
skopeo generate-sigstore-key --output-prefix "$work/registry/sigstore" \
  --passphrase-file "$work/registry/sigstore.pass" >/dev/null \
  || die 'cannot make the signing key the registry update is signed with'
mv "$work/registry-signed/image.oci.tar" "$work/registry/update.oci.tar"
staged_digest=$(cat "$work/registry-signed/digest")
cp "$work/registry/sigstore.pub" "$work/mnt/registry-update.pub"
cp os/tests/registry_update_check.sh "$work/mnt/registry_update_check.sh"
umount "$work/mnt"
rm -rf "$work/update-signed" "$work/registry-signed" "$work/unsigned"
qemu-img convert -f raw -O qcow2 "$work/update.raw" "$work/update.qcow2"
rm -f "$work/update.raw"
# The floor's authority runs the image's own tilo-rollback on this side: it drives no TPM
# and links nothing but the C library.
podman create --name host-assurance-rollback localhost/tilo-host:ci >/dev/null
podman cp host-assurance-rollback:/usr/libexec/tilo/tilo-rollback "$work/tilo-rollback"
podman rm host-assurance-rollback >/dev/null
printf 'n=%s\nstaged_digest=%s\nfollows=%s\n' "$n" "$staged_digest" "main-${target:0:7}-ci" > "$work/base.vars"
cp "$work/install/qcow2/recovery-key.txt" "$work/recovery-key.txt"
podman rmi -f "$update_ref" localhost/tilo-host:sealed localhost/tilo-host:ci localhost/tilo-host:seal >/dev/null 2>&1 || true
rm -rf "$work/image.oci" "$work/sealed" "$work/signed" "$work/loader-signed" "$work/trusted-image" \
  "$work/older-built" "$work/older-sealed" "$work/older-signed" "$work/install" "$work/guest.qcow2"
}

# The same update served by a registry on this machine, which the guest reaches
# as 10.0.2.2 and is told is a mirror of registry.tilo.so/tilo/tilo-host: under
# the tag the server was installed to follow, signed with a key made for the
# repository's name, and beside it an image of the repository nobody signed
# (os/tests/registry_update_check.sh says what the guest does with them).
start_registry() {
  podman run -d --name host-assurance-registry -p 127.0.0.1:5050:5000 "$SCRATCH_REGISTRY_IMAGE" >/dev/null \
    || die 'cannot start the update registry'
  for _ in $(seq 30); do curl -fs http://127.0.0.1:5050/v2/ >/dev/null && break; sleep 1; done
  # Signatures go beside the image as sigstore attachments, as the release
  # registry keeps them (os/etc/containers/registries.d).
  mkdir -p "$work/registries.d"
  printf 'docker:\n  127.0.0.1:5050:\n    use-sigstore-attachments: true\n' > "$work/registries.d/update.yaml"
  skopeo --registries.d "$work/registries.d" copy -q --preserve-digests --dest-tls-verify=false \
    --sign-by-sigstore-private-key "$work/registry/sigstore.private" \
    --sign-passphrase-file "$work/registry/sigstore.pass" \
    --sign-identity "registry.tilo.so/tilo/tilo-host:$follows" \
    "oci-archive:$work/registry/update.oci.tar" "docker://127.0.0.1:5050/tilo/tilo-host:$follows" \
    || die 'cannot publish the signed update to the registry'
  mkdir -p "$work/unsigned-image"
  printf 'an image nobody signed\n' > "$work/unsigned-image/note"
  printf 'FROM scratch\nCOPY note /note\n' > "$work/unsigned-image/Containerfile"
  podman build -q -t localhost/host-assurance-unsigned -f "$work/unsigned-image/Containerfile" "$work/unsigned-image" >/dev/null
  skopeo copy -q --dest-tls-verify=false containers-storage:localhost/host-assurance-unsigned \
    docker://127.0.0.1:5050/tilo/tilo-host:unsigned || die 'cannot publish the unsigned image to the registry'
}

# A stage's machine: the base's disk and update disk as overlays it writes to, and copies of
# its TPM and firmware variables. The update disk takes this tree's guest-side scripts, so a
# fix to one is proved without a new base.
restore_base() {
  local base=$base_in/base
  for part in disk.qcow2 update.qcow2 state/base.vars; do
    [[ -s $base/$part ]] || die "the base in $base has no $part: the stage needs host-assurance-base-fetch"
  done
  cp -a "$base/state/." "$work/"
  qemu-img create -q -f qcow2 -F qcow2 -b "$(realpath "$base/disk.qcow2")" "$work/disk.qcow2"
  qemu-img convert -f qcow2 -O raw "$base/update.qcow2" "$work/update.raw"
  mkdir -p "$work/mnt"
  mount -o loop "$work/update.raw" "$work/mnt"
  cp os/tests/registry_update_check.sh "$work/mnt/registry_update_check.sh"
  umount "$work/mnt"
  qemu-img convert -f raw -O qcow2 "$work/update.raw" "$work/update.qcow2"
  rm -f "$work/update.raw"
  ln -s "$(realpath "$base/registry")" "$work/registry"
}

if [[ $mode == stage ]]; then
  step restore
  restore_base
else
  prepare
fi
# shellcheck source=/dev/null
. "$work/base.vars"
disk=$work/disk.qcow2
TILO_UNLOCK_SECRET=$(cat "$work/recovery-key.txt")
[[ -n $TILO_UNLOCK_SECRET ]] || die "the install's recovery key file is empty"
export TILO_UNLOCK_SECRET TILO_DISTROBOX="" TILO_ACCEL=kvm TILO_USER TILO_PASS
export TILO_TPM_DIR=$work/tpm

export TILO_RUN=$work/run TILO_DISK=$disk
export TILO_SERIAL_SOCKET=$work/run/serial.sock
export TILO_CONSOLE_LOG=$out/console.log TILO_CONSOLE_USER=$TILO_USER TILO_CONSOLE_PASS=$TILO_PASS
# One line may import a gigabyte into the machine's storage (bootc switch) or
# check on a service that is still fetching its guest; the script's own waits
# are the bound on those, not the console runner's default for one command.
export TILO_CONSOLE_DEADLINE=1800
mkdir -p "$TILO_RUN"
# The machine under test boots with the TPM the install guest had: a keyslot the
# image carries a signed boot chain for is sealed to this one device. Its firmware
# enforces Secure Boot with the throwaway EFI certificate as its only key.
export TILO_SECBOOT_KEYS=$work/secureboot-vars.qcow2
boot_vm() {
  ./os/dev/vm.sh boot --2d --tpm --disk "$disk" --attach "$work/update.qcow2"
}
finish() {
  local code=$?
  report_exit "$code"
  ./os/dev/vm.sh serial > "$out/serial.log" 2>&1 || true
  # A failed check keeps no outputs, and a boot that hangs says nothing of its own:
  # what the machine last printed goes to the log.
  if [ "$code" != 0 ]; then
    # A boot that hangs before its console says anything shows only on its screen, and a
    # firmware or boot loader stalled on the TPM leaves its trace in swtpm's log.
    ./os/dev/vm.sh shot failure >/dev/null 2>&1 && cp "${TILO_RUN:-}/failure.png" "$out/" 2>/dev/null || true
    cp "${TILO_TPM_DIR:-}/swtpm.log" "$out/" 2>/dev/null || true
    { printf '\n--- the machine'"'"'s console, its last 100 lines\n'
      tail -n 100 "$out/serial.log" | tr -d '\r' | sed 's/\x1b\[[0-9;?]*[A-Za-z]//g'
    } 2>/dev/null | tee /dev/console >&2 2>/dev/null || true
  fi
  ./os/dev/vm.sh stop >/dev/null 2>&1 || true
  podman rm -f host-assurance-registry >/dev/null 2>&1 || true
  table
}
trap finish EXIT
console() { ./os/tests/serial_runner.sh "$1"; }
as_root() { console "printf '%s\n' '$TILO_PASS' | sudo -S -p '' bash -c $(printf '%q' "$1")"; }
ok()   { printf 'ok    %s\n' "$*" >&2; }
# A failed check keeps no outputs, so what the machine answered goes to the log.
refuse() {
  local what="$1"; shift
  as_root "$@" > "$out/$what.txt" 2>&1 || { tail -40 "$out/$what.txt" >&2; die "FAIL $what"; }
  ok "$what"
}

# The prompt comes before any network, so the serial console is the only input,
# and the console runner cannot be used to ask whether the guest is up: while
# the volume is closed it waits out its own deadline. What this boot printed on
# the serial line is the signal instead: a login prompt in it means the volume
# opened.
unlock_until_console() {
  local tries="${1:-120}"
  for _ in $(seq 1 "$tries"); do
    ./os/dev/unlock-prompt.sh "$TILO_UNLOCK_SECRET" >/dev/null 2>&1 || true
    if ./os/dev/vm.sh this-boot | grep -qi 'login:'; then
      console 'true' >/dev/null 2>&1 && return 0
    fi
    sleep 3
  done
  return 1
}
# A boot whose volume the recovery key opened, answering at its console.
boot_settled() {
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
}

stage_serves() {
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
# What the mapper is, from the kernel: a dm-crypt device carrying LUKS2 says
# so in its uuid (CRYPT-LUKS2-<volume>-<name>; LUKS1 says CRYPT-LUKS1).
uuid=""
for dm in /sys/block/dm-*; do
  if [ "$(cat "$dm/dm/name" 2>/dev/null)" = "$name" ]; then
    uuid=$(cat "$dm/dm/uuid" 2>/dev/null)
  fi
done
case "$uuid" in
  CRYPT-LUKS2-*) ;;
  *) echo "the mapping $name is not a LUKS2 crypt device (uuid: ${uuid:-none})"; exit 1 ;;
esac
backing=""
while read -r n t; do
  [ "$t" = part ] || continue
  backing=$n
  break
done <<EOF
$(lsblk -snro NAME,TYPE "$src")
EOF
[ -n "$backing" ] || { echo "no partition under $src"; lsblk -s "$src"; exit 1; }
[ "$(blkid -p -o value -s TYPE "/dev/$backing")" = crypto_LUKS ] \
  || { echo "/dev/$backing carries no LUKS header"; blkid "/dev/$backing"; exit 1; }
echo "the root is $src: $uuid over /dev/$backing"
'
refuse 'the root has the recovery key and no throwaway install key' '
set -eu
src=$(findmnt -nro SOURCE /sysroot 2>/dev/null || findmnt -nro SOURCE /)
command -v cryptsetup >/dev/null \
  || { echo "the image carries no cryptsetup, so its own LUKS2 header cannot be read here"; exit 1; }
# The mapper holds the plaintext view; the header is on the partition under it.
backing=$(lsblk -snro NAME,TYPE "$src" | awk '"'"'$2 == "part" { print $1; exit }'"'"')
[ -n "$backing" ] || { echo "no partition under $src"; lsblk -s "$src"; exit 1; }
dump=$(cryptsetup luksDump "/dev/$backing")
printf "%s\n" "$dump" | grep -q systemd-recovery \
  || { echo "no systemd-recovery token in the header"; exit 1; }
slots=$(printf "%s\n" "$dump" | sed -n "/^Keyslots:/,/^Tokens:/p" | grep -cE "^  [0-9]+:")
[ "$slots" = 1 ] \
  || { echo "$slots keyslots; the throwaway install key was not removed"; exit 1; }
echo "one keyslot: the recovery key the install handed over"
'
refuse 'the store volume is LUKS2 and mounts' '
set -eu
uuid=""
for dm in /sys/block/dm-*; do
  if [ "$(cat "$dm/dm/name" 2>/dev/null)" = tilo-store ]; then
    uuid=$(cat "$dm/dm/uuid" 2>/dev/null)
  fi
done
case "$uuid" in
  CRYPT-LUKS2-*) ;;
  *) echo "tilo-store is not an open LUKS2 volume (uuid: ${uuid:-none})"; exit 1 ;;
esac
findmnt -nro FSTYPE /var/lib/tilo/store | grep -qx btrfs
grep -q tilo-store /etc/crypttab
echo "the store is a LUKS2 + btrfs volume ($uuid), unlocked from the root"
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

say 'the boot is hardened'
# What the image asked for is what this boot does: the kernel locked itself
# down from the boot entry's arguments, nothing merges over the image, nobody but
# the account this test disk was given signs in, sudo is the only setuid
# program, and nothing listens beyond the machine but its network manager and
# Tilo's own transport.
refuse 'the kernel is locked down and the boot entry carries the hardening arguments' '
set -eu
grep -q "\[integrity\]" /sys/kernel/security/lockdown \
  || { echo "lockdown: $(cat /sys/kernel/security/lockdown)"; exit 1; }
for karg in lockdown=integrity init_on_alloc=1 slab_nomerge vsyscall=none systemd.import_credentials=no; do
  grep -qw -- "$karg" /proc/cmdline || { echo "no $karg in: $(cat /proc/cmdline)"; exit 1; }
done
echo "lockdown: $(cat /sys/kernel/security/lockdown)"
'
refuse 'nothing merges an extension over the image at boot' '
set -eu
for u in systemd-sysext.service systemd-confext.service systemd-sysext.socket; do
  state=$(systemctl is-enabled "$u" 2>/dev/null || true)
  [ "$state" = masked ] || { echo "$u is $state"; exit 1; }
done
'
refuse 'only the account this test disk was given has a password' "
set -eu
open=\$(awk -F: '\$2 !~ /^[!*]/ { print \$1 }' /etc/shadow)
[ \"\$open\" = '$TILO_USER' ] || { echo \"accounts with a password: \$open\"; exit 1; }
"
refuse 'sudo is the only setuid program' '
set -eu
found=$(find /usr -xdev -type f -perm /6000 | sort)
[ "$found" = /usr/bin/sudo ] || { echo "setuid or setgid: $found"; exit 1; }
'
refuse 'the firewall opens nothing but DHCPv6 replies' '
set -eu
[ "$(firewall-cmd --get-default-zone)" = FedoraServer ]
[ "$(firewall-cmd --list-services)" = dhcpv6-client ] \
  || { echo "services: $(firewall-cmd --list-services)"; exit 1; }
'
refuse 'nothing listens beyond this machine but the network manager and Tilo' '
set -eu
ss -H -tulnp > /run/host-assurance-listeners
cat /run/host-assurance-listeners
beyond=$(grep -Ev "^[a-z]+ +[A-Z]+ +[0-9]+ +[0-9]+ +(127\.[0-9.]+|\[::1\]|\[::ffff:127\.[0-9.]+\])(%[^:]+)?:" /run/host-assurance-listeners || true)
odd=$(printf "%s\n" "$beyond" | grep -Ev "users:\(\(\"(NetworkManager|tilo-server|fs)\"" | grep . || true)
[ -z "$odd" ] || { echo "listening beyond this machine: $odd"; exit 1; }
tcp=$(printf "%s\n" "$beyond" | grep "^tcp" || true)
[ -z "$tcp" ] || { echo "a TCP port beyond loopback: $tcp"; exit 1; }
'
# The boot-entry pass ran 30 seconds into the boot and turned the boot menu's
# editor off, which bootc leaves on for kernel/initrd entries.
refuse 'the boot menu edits no kernel arguments' '
set -eu
systemctl is-active --quiet tilo-boot-entries.service \
  || { systemctl status --no-pager tilo-boot-entries.service; exit 1; }
cat /boot/loader/loader.conf
[ "$(grep -E "^ *editor( |\$)" /boot/loader/loader.conf)" = "editor no" ]
'

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

say 'the boot is verified and measured'
# The firmware enforces Secure Boot with the throwaway EFI certificate as its
# only key, and booted this machine: through systemd-stub, from the signed UKI
# whose command line carries the generation the image does, measured into PCR 11.
refuse 'the firmware enforced Secure Boot and booted the signed UKI' '
set -eu
var() { od -An -t u1 -j4 -N1 "/sys/firmware/efi/efivars/$1" | tr -d " "; }
[ "$(var SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c)" = 1 ] || { echo "Secure Boot is off"; exit 1; }
[ "$(var SetupMode-8be4df61-93ca-11d2-aa0d-00e098032b8c)" = 0 ] || { echo "the firmware is in setup mode"; exit 1; }
ls /sys/firmware/efi/efivars/StubPcrKernelImage-4a67b082-0a4c-41cf-b6c7-440b29bb8c4f >/dev/null \
  || { echo "systemd-stub did not start this kernel"; exit 1; }
generation=$(cat /usr/lib/tilo/generation)
grep -qw "tilo.generation=$generation" /proc/cmdline || { echo "no tilo.generation=$generation in: $(cat /proc/cmdline)"; exit 1; }
pcr=$(tpm2_pcrread sha256:11 | sed -n "s/^ *11 *: *0x//p")
case "$pcr" in ""|*[!0-9A-Fa-f]*|0000000000000000000000000000000000000000000000000000000000000000) tpm2_pcrread sha256:7,11; exit 1 ;; esac
echo "generation $generation, PCR 11 $pcr"
'
}

stage_refused() {
# A boot the firmware must refuse. What breaks it is done as root on the
# running machine, as anyone who can write the ESP could, in a mount namespace
# of its own where the ESP's read-only bind is made writable, as the services
# that write it do. The machine is then started again and must not come up, and
# the boot loader must say the firmware refused the image; the disk then goes
# back to how it was before.
as_root 'sync' >/dev/null 2>&1 || true
./os/dev/vm.sh stop >/dev/null 2>&1 || true
qemu-img snapshot -c signed "$disk" || die 'cannot keep the signed disk to go back to'
refused_boot() {  # WHAT FILE ROOT-SCRIPT
  local what=$1 file=$2 script=$3
  boot_vm
  unlock_until_console || die "the signed disk never answered at its console before: $what"
  as_root "dmesg -n 1; unshare -m sh -euc $(printf '%q' "mount -o remount,bind,rw /boot; $script; sync")" \
    > "$out/$file-broken.txt" 2>&1 || { tail -40 "$out/$file-broken.txt" >&2; die "could not break the boot entry: $what"; }
  ./os/dev/vm.sh stop >/dev/null 2>&1 || true
  boot_vm
  # Its unlock prompt comes before any getty: a minute and a half with neither on
  # the console is a machine that did not start.
  if TILO_CONSOLE_PROMPT_TIMEOUT=90 console 'true' >/dev/null 2>&1; then
    ./os/dev/vm.sh serial > "$out/$file-serial.log" 2>&1 || true
    die "FAIL $what: the machine booted"
  fi
  ./os/dev/vm.sh serial > "$out/$file-serial.log" 2>&1 || true
  ./os/dev/vm.sh stop >/dev/null 2>&1 || true
  # The boot menu draws its messages by cursor position: read the text without the codes.
  sed 's/\x1b\[[0-9;]*H/ /g; s/\x1b\[[0-9;?=]*[A-Za-z]//g' "$out/$file-serial.log" | tr -d '\r' \
    > "$out/$file-console.txt"
  grep -aiqE 'security violation|access denied' "$out/$file-console.txt" \
    || { grep -a . "$out/$file-console.txt" | cut -c1-300 >&2
         die "FAIL $what: nothing on the console says the firmware refused it"; }
  qemu-img snapshot -a signed "$disk" || die 'cannot put the signed disk back'
  ok "$what"
}
uki='uki=$(ls /boot/EFI/Linux/bootc/*.efi); [ "$(printf "%s\n" $uki | wc -l)" = 1 ]'
refused_boot 'a UKI whose command line was edited is refused' edited-cmdline \
  "$uki; /usr/libexec/tilo-uki-cmdline set tilo.mode=repair \$uki; /usr/libexec/tilo-uki-cmdline show \$uki | grep -qw tilo.mode=repair"
refused_boot 'a UKI nobody signed is refused' unsigned-uki \
  "$uki; mkdir -p /mnt/update; mountpoint -q /mnt/update || mount -o ro /dev/vdb /mnt/update; cp /mnt/update/unsigned.efi \$uki"
qemu-img snapshot -d signed "$disk" || true

boot_vm
unlock_until_console || die 'the signed disk never answered at its console again'
as_root 'dmesg -n 1; systemctl stop tilo-server-firstboot.service' >/dev/null 2>&1 || true
for _ in $(seq 1 90); do
  as_root 'systemctl is-active --quiet multi-user.target' && break
  sleep 5
done
refuse 'the signed disk boots again as it was' 'systemctl is-active --quiet multi-user.target'
}

stage_rollback() {
# The server probe runs when a boot is judged, and the boot loader counts only an
# update's boots, so nothing has run it on this one. The rollback below breaks only
# the staged deployment, and needs the probe to have said this one is good first.
refuse 'the server probe judges the signed disk good' 'systemctl start tilo-server-health.service'

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
     RESET="./os/dev/vm.sh stop; ./os/dev/vm.sh boot --2d --tpm --disk '$disk' --attach '$work/update.qcow2'; ./os/dev/unlock-prompt.sh || true" \
     SERIAL='./os/dev/vm.sh serial' \
     ANSWERS="./os/dev/vm.sh this-boot | grep -q 'login: '" \
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
}

stage_registry() {
say 'a registry update is verified and staged'
start_registry
refuse 'a registry update is verified and staged, and an unsigned image refused' "
set -eu
mkdir -p /mnt/update
mkdir -p /mnt/update; mountpoint -q /mnt/update || mount -o ro /dev/vdb /mnt/update
bash /mnt/update/registry_update_check.sh 10.0.2.2:5050/tilo/tilo-host /mnt/update/registry-update.pub unsigned '$staged_digest'
"
}

machine_sealed=0
stage_seal() {
say 'the disk is sealed to this machine and opens by itself'
# The installer enrolled the TPM keyslot in a guest of its own, whose PCR 7 is
# not this machine's, so that slot was never going to open here. The installed
# machine seals its own: the shipped tilo-rollback enrol, against the signed
# PCR 11 prediction of the generation it runs and this machine's live PCR 7 —
# the seal tilo-disk-encrypt enrol makes on a machine the installer runs on.
# What userspace measured into PCR 11 after the boot image: a seal checked against the live
# PCRs is checked against the phases measured since the initrd started.
as_root 'echo "PCR 11 now: $(tpm2_pcrread sha256:11 | sed -n "s/^ *11 *: *//p")"
grep -ao "\"string\":\"[^\"]*\"" /run/log/systemd/tpm2-measure.log 2>/dev/null | tr "\n" " "; echo
systemctl is-enabled systemd-pcrphase.service systemd-pcrphase-initrd.service systemd-pcrphase-sysinit.service 2>&1 | tr "\n" " "' >&2 || true
refuse 'this machine seals the volume it booted, with no secret' '
set -eu
mkdir -p /mnt/update; mountpoint -q /mnt/update || mount -o ro /dev/vdb /mnt/update
src=$(findmnt -nro SOURCE /sysroot 2>/dev/null || findmnt -nro SOURCE /)
dev=/dev/$(lsblk -snro NAME,TYPE "$src" | awk '"'"'$2 == "part" { print $1; exit }'"'"')
gen=$(cat /usr/lib/tilo/generation)
/usr/libexec/tilo/tilo-rollback enroll --device "$dev" --generation "$gen" \
  --predictions /mnt/update/predictions --recovery-key-file /mnt/update/recovery.key --automatic
/usr/libexec/tilo/tilo-rollback status --device "$dev"
'
./os/dev/vm.sh stop >/dev/null 2>&1 || true
boot_vm
# Nothing is typed from here: the boot reaching the console's login prompt is
# the volume opening by itself, which is what the PIN-less keyslot is for.
unattended_unlock() {
  local tries=${1:-120}
  for _ in $(seq 1 "$tries"); do
    # Only what this boot printed: a prompt from an earlier boot says nothing
    # about the volume this one opened. Either a login prompt or a session
    # already signed in counts, as bash prints it with or without the
    # account-and-host bracket.
    if ./os/dev/vm.sh this-boot | grep -Eq 'login: |(^\[[^]]*\])?[#$] '; then
      console 'true' >/dev/null 2>&1 && return 0
    fi
    sleep 3
  done
  return 1
}
unattended_unlock || { ./os/dev/vm.sh serial > "$out/unattended-serial.log" 2>&1 || true
                       die 'the disk asked for a secret on a machine sealed to it: the PIN-less keyslot did not unseal'; }
# The initrd's journal is its own; the boot's messages reach this side through
# the console, so either place the unlock recorded itself counts.
as_root 'journalctl -b --no-pager 2>/dev/null' > "$out/unattended-journal.txt" 2>&1 || true
if grep -q 'unsealed to /run/cryptsetup-keys.d/root.key' "$out/unattended-journal.txt" \
   || ./os/dev/vm.sh this-boot | grep -q 'unsealed to /run/cryptsetup-keys.d/root.key'; then
  ok 'the boot unlocked through the TPM keyslot, not a typed secret'
else
  ./os/dev/vm.sh this-boot | tail -40 >&2 || true
  die 'the disk opened by itself, but nothing recorded the TPM keyslot as what opened it'
fi
# The machine went down by a power cut (vm.sh stop), right after the registry stage
# staged an update: what bootc wrote to the ESP for it has to be on the disk, or
# every later `bootc status`, and the updates and fallbacks reading it, fail.
refuse 'the boot entries written before a power cut are whole after it' '
set -eu
bootc status --format json >/dev/null
for unit in tilo-boot-entries.service tilo-boot-fallback.service; do
  if systemctl is-failed --quiet "$unit"; then
    journalctl -b --no-pager -u "$unit" | tail -20
    echo "$unit failed on this boot"; exit 1
  fi
done
'
as_root 'dmesg -n 1; systemctl stop tilo-server-firstboot.service' >/dev/null 2>&1 || true
machine_sealed=1
}

stage_floor() {
(( machine_sealed )) || die 'the floor stage runs on a disk this machine sealed: run the seal stage before it'
say 'an image below the floor is refused'
# The older signed image the update disk carries (generation 0) gets a keyslot
# of its own, then the floor moves to this generation: the machine must refuse
# to unseal it, and the recovery key must still open the disk.
refuse 'this machine has a keyslot for the older generation' "
set -eu
mkdir -p /mnt/update; mountpoint -q /mnt/update || mount -o ro /dev/vdb /mnt/update
src=\$(findmnt -nro SOURCE /sysroot 2>/dev/null || findmnt -nro SOURCE /)
dev=/dev/\$(lsblk -snro NAME,TYPE \"\$src\" | awk '\$2 == \"part\" { print \$1; exit }')
/usr/libexec/tilo/tilo-rollback enroll --device \"\$dev\" --generation 0 \
  --predictions /mnt/update/older-predictions --recovery-key-file /mnt/update/recovery.key --automatic
/usr/libexec/tilo/tilo-rollback status --device \"\$dev\"
"
# The floor moves by a signed, single-use approval per increment: requested on
# the machine, issued by the authority (the throwaway PCR key, here on the
# check's side), brought back and spent by confirm.
request=$(as_root '
set -eu
src=$(findmnt -nro SOURCE /sysroot 2>/dev/null || findmnt -nro SOURCE /)
dev=/dev/$(lsblk -snro NAME,TYPE "$src" | awk '"'"'$2 == "part" { print $1; exit }'"'"')
/usr/libexec/tilo/tilo-rollback floor-request --device "$dev" | base64 -w0
') || die 'the machine could not write a floor request'
printf '%s' "$request" | base64 -d > "$work/floor-request.json" \
  || die 'the floor request did not survive the console'
mkdir -p "$work/approvals"
"$work/tilo-rollback" approve-floor --private-key "$work/boot-keys/pcr.key" \
  --request "$work/floor-request.json" --out-dir "$work/approvals" \
  || die 'the authority could not approve the floor advance'
shopt -s nullglob
approvals=("$work"/approvals/*.json)
shopt -u nullglob
(( ${#approvals[@]} > 0 )) || die 'the authority issued no floor approval'
for approval in "${approvals[@]}"; do
  as_root "mkdir -p /var/lib/tilo/rollback/approvals
printf '%s' '$(base64 -w0 < "$approval")' | base64 -d > /var/lib/tilo/rollback/approvals/$(basename "$approval")" \
    || die "could not put $(basename "$approval") on the machine"
done
refuse 'the floor moves to this generation, with the approvals' "
set -eu
mkdir -p /mnt/update; mountpoint -q /mnt/update || mount -o ro /dev/vdb /mnt/update
src=\$(findmnt -nro SOURCE /sysroot 2>/dev/null || findmnt -nro SOURCE /)
dev=/dev/\$(lsblk -snro NAME,TYPE \"\$src\" | awk '\$2 == \"part\" { print \$1; exit }')
gen=\$(cat /usr/lib/tilo/generation)
/usr/libexec/tilo/tilo-rollback --retain \$((gen + 1)) --predictions /mnt/update/predictions \
  confirm --device \"\$dev\" --generation \"\$gen\" --floor-at-least \"\$gen\" \
  --recovery-key-file /mnt/update/recovery.key
/usr/libexec/tilo/tilo-rollback status --device \"\$dev\"
/usr/libexec/tilo/tilo-rollback status --device \"\$dev\" --json
"
grep -q "\"floor_generation\":$n" "$out/the floor moves to this generation, with the approvals.txt" \
  || die "the floor did not move to generation $n"
# Put the older signed image on the ESP in place of this one, and start it. A machine that
# refuses it stops at its recovery prompt, so the way back is the disk as it was before the
# swap; the floor lives in the TPM, which keeps it.
as_root 'sync' >/dev/null 2>&1 || true
./os/dev/vm.sh stop >/dev/null 2>&1 || true
qemu-img snapshot -c floor "$disk" || die 'cannot keep the disk to go back to after the older image'
boot_vm
unattended_unlock || die 'the sealed disk did not open by itself before the older image went on it'
as_root 'dmesg -n 1; systemctl stop tilo-server-firstboot.service' >/dev/null 2>&1 || true
refuse 'the older signed image is on the ESP' '
set -eu
mkdir -p /mnt/update; mountpoint -q /mnt/update || mount -o ro /dev/vdb /mnt/update
# The UKI this boot started: the one whose command line names its composefs image (an
# update the registry stage staged has a UKI of its own beside it).
booted=$(tr " " "\n" < /proc/cmdline | sed -n "s/^composefs=//p")
uki=""
for f in /boot/EFI/Linux/bootc/*.efi; do
  /usr/libexec/tilo-uki-cmdline show "$f" 2>/dev/null | tr " " "\n" | grep -qx "composefs=$booted" && uki="$uki $f"
done
[ "$(printf "%s\n" $uki | grep -c .)" = 1 ] || { echo "no single UKI boots composefs=$booted: $(ls /boot/EFI/Linux/bootc)"; exit 1; }
unshare -m sh -euc "mount -o remount,bind,rw /boot; cp /mnt/update/older.efi $uki; sync"
'
as_root 'sync' >/dev/null 2>&1 || true
./os/dev/vm.sh stop >/dev/null 2>&1 || true
boot_vm
if TILO_CONSOLE_PROMPT_TIMEOUT=120 console 'true' >/dev/null 2>&1; then
  ./os/dev/vm.sh serial > "$out/floor-serial.log" 2>&1 || true
  die 'the machine unlocked an image below the floor'
fi
./os/dev/vm.sh serial > "$out/floor-serial.log" 2>&1 || true
grep -q 'rollback floor refuses generation 0' "$out/floor-serial.log" \
  || { tail -40 "$out/floor-serial.log" >&2
       die 'the image below the floor did not open, but nothing on the console says the floor refused it'; }
# The same disk, back on the image the machine runs: the floor permits this
# generation, so it opens by itself again. The floor is what differed.
./os/dev/vm.sh stop >/dev/null 2>&1 || true
qemu-img snapshot -a floor "$disk" || die 'cannot put the disk back as it was before the older image'
qemu-img snapshot -d floor "$disk" || true
boot_vm
unattended_unlock || { ./os/dev/vm.sh serial > "$out/floor-permitted-serial.log" 2>&1 || true
                       die 'the signed image the machine runs no longer opens its disk by itself'; }
console 'true' >/dev/null || die 'the restored image never answered at its console'
refuse 'the floor still permits the running generation and refuses the older one' '
set -eu
src=$(findmnt -nro SOURCE /sysroot 2>/dev/null || findmnt -nro SOURCE /)
dev=/dev/$(lsblk -snro NAME,TYPE "$src" | awk '"'"'$2 == "part" { print $1; exit }'"'"')
/usr/libexec/tilo/tilo-rollback status --device "$dev"
/usr/libexec/tilo/tilo-rollback status --device "$dev" --json | grep -q "\"floor_generation\":$(cat /usr/lib/tilo/generation)" \
  || { echo "the floor is not at the running generation"; exit 1; }
'

}

all_stages=(serves refused rollback registry seal floor)
if [[ $mode == stage ]]; then
  for st in "${stages[@]}"; do
    declare -F "stage_$st" >/dev/null || die "no stage $st; the stages are ${all_stages[*]}"
  done
fi
case $mode in
  full)
    step first-boot
    boot_settled
    for st in "${all_stages[@]}"; do step "$st"; "stage_$st"; done
    ;;
  base)
    step first-boot
    boot_settled
    step serves
    stage_serves
    # The base is the machine as its first boot left it, shut down.
    as_root 'sync' >/dev/null 2>&1 || true
    ./os/dev/vm.sh stop >/dev/null 2>&1 || true
    step keep
    rm -rf "$base_out" && mkdir -p "$base_out/state"
    mv "$work/disk.qcow2" "$work/update.qcow2" "$base_out/"
    mv "$work/registry" "$base_out/registry"
    cp -a "$work/secureboot-vars.qcow2" "$work/tpm" "$work/boot-keys" "$work/recovery-key.txt" \
      "$work/tilo-rollback" "$work/base.vars" "$base_out/state/"
    du -sh --apparent-size "$base_out"/* >&2
    printf '\nthe base is in %s\n' "$base_out" >&2
    exit 0
    ;;
  stage)
    step boot
    boot_settled
    for st in "${stages[@]}"; do step "$st"; "stage_$st"; done
    printf '\nPASS: %s, on the base %s\n' "${stages[*]}" "$base_in" >&2
    exit 0
    ;;
esac

printf '\nPASS: the server image installs onto an encrypted disk, boots its release-signed boot loader and signed UKI under Secure Boot and refuses an edited or unsigned one, serves, rolls back an update that breaks it, verifies and stages a registry update, opens its disk by itself through the signed PCR 11 prediction and this machine'"'"'s PCR 7, and refuses an image below the rollback floor (evidence in %s)\n' "$out"

