#!/usr/bin/env bash
# The server image proves itself by booting, in a VM: the machine boots its
# signed UKI under Secure Boot and refuses one whose command line was edited or
# that nobody signed, comes up serving (the hosted-VM service binds its socket,
# the boot judges itself, an update stages through the verified path), an
# update that breaks the server rolls itself back without anyone at the machine,
# and an update from a registry is verified before it stages.
#
# The disk is the image seal-host sealed, its boot chain signed as a release
# signs it (os-release.sh sign-boot, host-seal.sh apply) with throwaway EFI and
# PCR keys, installed the way the desktop's dev disk is (os/dev/build-image.sh,
# bootc install to-filesystem) onto a LUKS2 root with a LUKS2 store, the layout
# the installer makes (TILO_ENCRYPT_INSTALL=1). That install opens the volume
# through device-mapper, which this check's micro-VM has not, so it runs in a
# guest of its own, booted by the same VM harness with a software TPM; the
# machine under test then boots that disk with the same TPM, under firmware that
# enforces Secure Boot with the throwaway EFI certificate as its only key, and
# the recovery key the install handed over is what opens it.
# The update staged on it is the same image with one more layer, sealed and
# signed the same way, so bootc sees a different deployment the firmware still
# accepts; os/tests/boot_assessment_rollback.sh then stages it, breaks only it
# and reboots until the boot loader has spent its tries and gone back, answering
# each boot's unlock prompt. The server image runs no sshd, so every step is
# driven at the serial console, which is also how a person reaches it
# (os/tests/serial_runner.sh).
#
# The record of every boot lands in /target/host-assurance.
set -euo pipefail
. tools/ci/publish/lib.sh

arch=x86_64
work=/var/tmp/host-assurance
out=${CARGO_TARGET_DIR:-/target}/host-assurance
sealed=/target/publish/host-sealed
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
[[ -s $sealed/image.oci.tar && -s $sealed/digest && -s $sealed/boot/message.bin ]] \
  || die "no sealed server image in $sealed: the check needs seal-host"

modprobe fuse 2>/dev/null || true
modprobe loop 2>/dev/null || true
test -c /dev/fuse || die 'no /dev/fuse, which the install needs'
test -c /dev/loop-control || die 'no loop devices, which the install needs'
losetup --find >/dev/null

say 'install the server image'
# What this step is given and what it has to put it in, so a failure here says
# which of the two ran out rather than only that the step died.
printf 'sealed image archive: %s bytes\n' "$(stat -c %s "$sealed/image.oci.tar")" >&2
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

# The boot chain signed as a release signs it (os-release.sh sign-boot, host-seal.sh apply), with
# throwaway EFI and PCR keys made here: the firmware below trusts those keys and nothing else.
export TILO_BOOT_SIGNER_DRY_RUN=$work/boot-keys TILO_BOOT_SIGNING_DRY_RUN=1
signed_image() {  # SEALED OUT: SEALED's boot chain signed, the image into OUT
  bash tools/ci/publish/os-release.sh sign-boot "$1" "$work/boot-signed" 2> >(tee /dev/console >&2) \
    || die "could not sign the boot chain of $1"
  bash tools/ci/publish/host-seal.sh apply "$1" "$work/boot-signed" "$2" 2> >(tee /dev/console >&2) \
    || die "could not add the signed boot chain to $1"
  rm -rf "$work/boot-signed"
}
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
# The install runs in a guest of the check's own, booted by the same VM harness
# the machine under test is: this check's micro-VM has no device-mapper (dm_mod
# does not load in it), and the encrypted layout opens the volume through one.
# The guest is this tree's dev disk — the machine the desktop's encrypted-install
# check installs from — and the command inside it is the one the check ran on its
# own disk before encryption: os/dev/build-image.sh with TILO_ENCRYPT_INSTALL=1,
# unchanged, so what is proved here is the shipped path and not a second one.
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
  util-linux jq python3 dosfstools btrfs-progs systemd mtools qemu-img sudo curl tar gzip zstd \\
  cryptsetup shadow-utils >/dev/null
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
# is still up. The key is this check's way into the machine at every boot until
# the image carries a signed boot chain for a TPM keyslot, which this one does
# not yet.
mkdir -p "$work/install"
ssh_guest "tar -C $guest_work/install -cf - qcow2/disk.qcow2 qcow2/recovery-key.txt" \
  | tar -C "$work/install" -xf - \
  || { stop_installer; die 'could not take the installed disk out of the guest'; }
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
# The boot loader the install put on the ESP is the base's systemd-boot, which no
# release signs yet: the throwaway EFI key signs it here, beside the UKIs the
# release chain signed, so the firmware's verdict below is about the UKIs.
esp_offset=$(sfdisk -J "$raw" | python3 -c 'import json, sys
table = json.load(sys.stdin)["partitiontable"]
esp = [p for p in table["partitions"] if p["type"].upper() == "C12A7328-F81F-11D2-BA4B-00A0C93EC93B"]
assert len(esp) == 1, esp
print(esp[0]["start"] * table.get("sectorsize", 512), esp[0]["size"] * table.get("sectorsize", 512))') \
  || die "cannot find the ESP on $raw"
read -r esp_start esp_size <<<"$esp_offset"
mkdir -p "$work/esp"
mount -o "loop,offset=$esp_start,sizelimit=$esp_size" "$raw" "$work/esp" || die 'cannot mount the ESP'
use_signing_tools
loaders=0
for loader in "$work"/esp/EFI/BOOT/BOOT*.EFI "$work"/esp/EFI/systemd/systemd-boot*.efi; do
  [[ -f $loader ]] || continue
  bash tools/signing/boot-signer.sh sign-efi "$loader" "$work/loader.efi" || die "cannot sign $loader"
  cp "$work/loader.efi" "$loader"
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
rpm -q python3-virt-firmware openssl >/dev/null 2>&1 \
  || dnf install -y --setopt=install_weak_deps=False python3-virt-firmware openssl >/dev/null
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
podman create --name host-assurance-unsigned localhost/tilo-host:sealed >/dev/null
podman cp host-assurance-unsigned:/boot/EFI/Linux/. "$work/unsigned"
podman rm host-assurance-unsigned >/dev/null
rm -f "$work/update.raw"
# The disk holds what the machine stages: the whole archive, and the layout
# skopeo unpacks it into beside the archive. Size it from the archive rather
# than a fixed number, which fails as a full filesystem on whichever image grew
# past it and names nothing that said why.
archive_mib=$(( ($(stat -c %s "$work/update-signed/image.oci.tar") + 1048575) / 1048576 ))
truncate -s "$(( 2 * archive_mib + 512 ))M" "$work/update.raw"
mkfs.ext4 -q -F -L tiloupdate "$work/update.raw"
mkdir -p "$work/mnt"
mount -o loop "$work/update.raw" "$work/mnt"
cp "$work/update-signed/image.oci.tar" "$work/mnt/update.oci.tar"
cp "$work"/unsigned/*.efi "$work/mnt/unsigned.efi"

# The same update served by a registry on this machine, which the guest reaches
# as 10.0.2.2 and is told is a mirror of registry.tilo.so/tilo/tilo-host: under
# the tag the server was installed to follow, signed with a key made here for
# the repository's name, and beside it an image of the repository nobody signed
# (os/tests/registry_update_check.sh says what the guest does with them).
podman run -d --name host-assurance-registry -p 127.0.0.1:5050:5000 docker.io/library/registry:2 >/dev/null \
  || die 'cannot start the update registry'
for _ in $(seq 30); do curl -fs http://127.0.0.1:5050/v2/ >/dev/null && break; sleep 1; done
printf 'host-assurance\n' > "$work/sigstore.pass"
skopeo generate-sigstore-key --output-prefix "$work/sigstore" --passphrase-file "$work/sigstore.pass" >/dev/null \
  || die 'cannot make the signing key the registry update is signed with'
follows="main-${target:0:7}-ci"
# Signatures go beside the image as sigstore attachments, as the release
# registry keeps them (os/etc/containers/registries.d).
mkdir -p "$work/registries.d"
printf 'docker:\n  127.0.0.1:5050:\n    use-sigstore-attachments: true\n' > "$work/registries.d/update.yaml"
skopeo --registries.d "$work/registries.d" copy -q --preserve-digests --dest-tls-verify=false \
  --sign-by-sigstore-private-key "$work/sigstore.private" --sign-passphrase-file "$work/sigstore.pass" \
  --sign-identity "registry.tilo.so/tilo/tilo-host:$follows" \
  "oci-archive:$work/update-signed/image.oci.tar" "docker://127.0.0.1:5050/tilo/tilo-host:$follows" \
  || die 'cannot publish the signed update to the registry'
staged_digest=$(cat "$work/update-signed/digest")
mkdir -p "$work/unsigned-image"
printf 'an image nobody signed\n' > "$work/unsigned-image/note"
printf 'FROM scratch\nCOPY note /note\n' > "$work/unsigned-image/Containerfile"
podman build -q -t localhost/host-assurance-unsigned -f "$work/unsigned-image/Containerfile" "$work/unsigned-image" >/dev/null
skopeo copy -q --dest-tls-verify=false containers-storage:localhost/host-assurance-unsigned \
  docker://127.0.0.1:5050/tilo/tilo-host:unsigned || die 'cannot publish the unsigned image to the registry'
cp "$work/sigstore.pub" "$work/mnt/registry-update.pub"
cp os/tests/registry_update_check.sh "$work/mnt/registry_update_check.sh"
umount "$work/mnt"
rm -rf "$work/update-signed" "$work/unsigned" "$work/unsigned-image"
qemu-img convert -f raw -O qcow2 "$work/update.raw" "$work/update.qcow2"
rm -f "$work/update.raw"
podman rmi -f "$update_ref" localhost/tilo-host:sealed localhost/tilo-host:ci localhost/tilo-host:seal >/dev/null 2>&1 || true

export TILO_RUN=$work/run TILO_DISK=$disk
export TILO_SERIAL_SOCKET=$work/run/serial.sock
export TILO_CONSOLE_LOG=$out/console.log TILO_CONSOLE_USER=$TILO_USER TILO_CONSOLE_PASS=$TILO_PASS
# One line may import a gigabyte into the machine's storage (bootc switch) or
# check on a service that is still fetching its guest; the script's own waits
# are the bound on those, not the console runner's default for one command.
export TILO_CONSOLE_DEADLINE=1800
# A secret sent before the prompt draws stays in the line buffer until it reads
# it, so the unlock is tried often rather than waited for.
export TILO_UNLOCK_DELAY=5
mkdir -p "$TILO_RUN"
# The machine under test boots with the TPM the install guest had: a keyslot the
# image carries a signed boot chain for is sealed to this one device. Its firmware
# enforces Secure Boot with the throwaway EFI certificate as its only key.
export TILO_SECBOOT_KEYS=$work/secureboot-vars.qcow2
boot_vm() {
  ./os/dev/vm.sh boot --2d --tpm --disk "$disk" --attach "$work/update.qcow2"
}
finish() {
  report_exit
  ./os/dev/vm.sh serial > "$out/serial.log" 2>&1 || true
  ./os/dev/vm.sh stop >/dev/null 2>&1 || true
  podman rm -f host-assurance-registry >/dev/null 2>&1 || true
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
$(lsblk -sno NAME,TYPE "$src")
EOF
[ -n "$backing" ] || { echo "no partition under $src"; lsblk -s "$src"; exit 1; }
[ "$(blkid -p -o value -s TYPE "/dev/$backing")" = crypto_LUKS ] \
  || { echo "/dev/$backing carries no LUKS header"; blkid "/dev/$backing"; exit 1; }
echo "the root is $src: $uuid over /dev/$backing"
'
refuse 'the root has the recovery key and no throwaway install key' '
set -eu
src=$(findmnt -nro SOURCE /sysroot)
name=${src#/dev/mapper/}
command -v cryptsetup >/dev/null \
  || { echo "the image carries no cryptsetup, so its own LUKS2 header cannot be read here"; exit 1; }
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
  grep -aiqE 'security violation|access denied' "$out/$file-serial.log" \
    || { tail -40 "$out/$file-serial.log" >&2; die "FAIL $what: nothing on the console says the firmware refused it"; }
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
say 'a registry update is verified and staged'
refuse 'a registry update is verified and staged, and an unsigned image refused' "
set -eu
mkdir -p /mnt/update
mountpoint -q /mnt/update || mount -o ro /dev/vdb /mnt/update
bash /mnt/update/registry_update_check.sh 10.0.2.2:5050/tilo/tilo-host /mnt/update/registry-update.pub unsigned '$staged_digest'
"
printf '\nPASS: the server image installs onto an encrypted disk, boots its signed UKI under Secure Boot and refuses an edited or unsigned one, serves, rolls back an update that breaks it, and verifies and stages a registry update (evidence in %s)\n' "$out"

