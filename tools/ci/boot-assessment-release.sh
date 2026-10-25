#!/usr/bin/env bash
# A broken update falls back by itself on the sealed release disk: the release
# image this tree builds, installed as a device installs it - /boot on the ESP
# itself, the UKI sealing the composefs digest the install computed - and booted
# under KVM.
#
#   tools/ci/boot-assessment-release.sh TARGET IMAGE OUT
#
# TARGET is an architecture (x86_64) or a board of release_boards in
# os/image/layers.toml; IMAGE is its release image's layout
# (/target/os/TARGET/release-image[-BOARD]), which must be this tree's build;
# OUT keeps the record of every boot.
#
# The dev-image lane (tools/ci/boot-assessment.sh) stages its image with one layer
# added, because a dev disk admits the unsigned repository it follows. A sealed
# disk does not: /etc/containers/policy.json makes registry.tilo.so/tilo/tilo -
# what a release installs from and updates from - require the release signature,
# and refuses the unsigned repositories outright. The update staged here is
# therefore the release itself, in the release's own repository under the guest's
# local storage (`containers-storage`, the transport policy admits because it
# fetches nothing), signed over that repository and digest as a published release
# is. This node holds no release key, so the signature is a throwaway one: what a
# sealed system decides here, it decides from the entry, its payload and the disk.
# Staging it is the same `bootc switch` a person runs; what happens after the
# reboot is the question this lane asks.
#
# On the installed release: every boot-entry pass succeeds, the staged
# deployment's entry is counted before its reboot, only that deployment is made
# unable to reach a greeter, each of its boots spends exactly one try - counted by
# the boot loader on a /boot that is the ESP, and blessed by systemd-bless-boot
# through the writable private remount that lets it rename an entry there - and
# after those tries the boot loader starts the release's own deployment again.
# os/tests/boot_assessment_rollback.sh does the checking; this script only gets a
# sealed disk into a VM and a staged update onto it.
#
# Inputs (t dispatch boot-assessment-release-x86_64 --input fault=...):
#   fault   greetd (default): greetd itself fails; greeter: greetd stays in
#           service while its greeter exits 12 s after each start; unlock: an
#           encrypted root's unlock prompt goes unanswered until the boot is
#           power-cycled there. A sealed release boots through a UKI, whose
#           command line cannot carry a fault, so unlock needs a disk whose root is
#           encrypted: name one with TILO_BOOT_ASSESSMENT_DISK.
set -euo pipefail

usage() {
  printf 'usage: %s TARGET IMAGE OUT\n' "${BASH_SOURCE[0]}" >&2
  exit 2
}

# TARGET from os/image/layers.toml: an architecture, or a board and its architecture.
resolve_target() {
  local target=$1 table=$2 arch board
  case $target in
    x86_64 | aarch64) arch=$target ;;
    *)
      grep -Eq "^release_boards = \[.*\"$target\"" "$table" \
        || { printf '%s is neither an architecture nor among release_boards in %s\n' "$target" "$table" >&2; exit 2; }
      arch=$(sed -n 's/^ *"architecture": *"\([^"]*\)".*/\1/p' "os/hardware/profiles/$target.json")
      board=$target ;;
  esac
  printf '%s %s\n' "${arch:-none}" "$board"
}

# The image directory under /target/os/ARCH for a target: the release image, or a board's.
image_dir_for() {
  printf 'release-image%s' "${2:+-$2}"
}

# Where a sealed system would find the release and its update: the repository
# /etc/containers/policy.json verifies, in the guest's own storage.
release_repository=registry.tilo.so/tilo/tilo
stage_name=$release_repository:sealed
update_name=$release_repository:staged
stage_reference=containers-storage:$stage_name
update_reference=containers-storage:$update_name

# A throwaway key under the release identity, in the release's own local
# references: this node holds no release signing key, and what a sealed system
# checks besides is the entry and its payload, which are the release's.
sign_staged() {
  local repository=$1 payload=$2 digest=$3 key=$scratch/release-signing
  [[ -d $key ]] || { mkdir -p "$key" && (cd "$key" && COSIGN_PASSWORD= cosign generate-key-pair >/dev/null); }
  printf '{"critical":{"identity":{"docker-reference":"%s"},"image":{"docker-manifest-digest":"%s"},"type":"cosign container image signature"},"optional":null}' \
    "$repository" "$digest" > "$payload"
  COSIGN_PASSWORD= cosign sign-blob --yes --tlog-upload=false --key "$key/cosign.key" \
    --output-signature "$payload.sig" "$payload" >/dev/null
  cosign verify-blob --insecure-ignore-tlog=true --key "$key/cosign.pub" --signature "$payload.sig" "$payload" >/dev/null
}

# FILES into the guest's /var/tmp, over the same SSH the guest's own account and
# the suite use.
scp_to_guest() {
  sshpass -p "$TILO_PASS" scp -q -P "${TILO_SSH_PORT:-2222}" -F /dev/null \
    -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o PreferredAuthentications=password -o PubkeyAuthentication=no \
    "$@" "$TILO_USER@127.0.0.1:$guest_scratch/"
}
# SCRIPT on the guest as root, through the user's own passcode, as a device's admin
# runs it.
as_root() {
  local script=$1
  shift
  "$vm" ssh "printf '%s\n' '$TILO_PASS' | sudo -S -p '' bash -c $(printf '%q' "$script") $(printf '%q ' "$@")"
}

# Tag the image in the guest as NAME, and put the signature of NAME under the
# local tag `sha256-<hex>` that installed systems read it from (containers/image's
# local signature store: $XDG_DATA_HOME/containers/signature, under /var for root).
# Every check is made where it is consumed: the image by the digest it must be, the
# signature over that repository and digest, and the store's name for that digest.
stage() {
  local name=$1 digest=$2 hex payload
  hex=${digest#sha256:}
  payload=$work/stage-payload.json
  sign_staged "${name%:*}" "$payload" "$digest"
  scp_to_guest "$payload" "$payload.sig"
  as_root "set -e
    test \"\$(podman image inspect --format '{{.Digest}}' "\$1")\" = \"\$2\"
    install -d -m 0755 /var/lib/containers/signature
    install -m 0644 \"\$3/\$4\" /var/lib/containers/signature/sha256-\$2
    install -m 0644 \"\$3/\$4.sig\" /var/lib/containers/signature/sha256-\$2.sig" \
    "$name" "$hex" "$guest_scratch" "$(basename "$payload")" || return 1
  rm -f "$payload" "$payload.sig"
}

if [[ ${1:-} != run ]]; then
  [[ $# -eq 3 ]] || usage
  read -r arch board <<< "$(resolve_target "$1" os/image/layers.toml)" || exit 2
  [[ $arch != none ]] || { printf '%s names no architecture\n' "$1" >&2; exit 2; }
  exec env TARGET_ARCH="$arch" TARGET_BOARD="$board" bash "${BASH_SOURCE[0]}" run "$1" "$2" "$3"
fi

target=${2:?usage: boot-assessment-release.sh TARGET IMAGE OUT}
image=${3:?}
out=${4:?}
arch=${TARGET_ARCH:-}
board=${TARGET_BOARD:-}
work=/var/tmp/boot-assessment-release
scratch=$work/scratch
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$repo_root"

fail() { printf 'FAIL  %s\n' "$*" >&2; exit 1; }
step() { printf '\n=== %s\n' "$*"; }
ok() { printf 'ok    %s\n' "$*"; }

[[ $arch == x86_64 ]] || fail "a sealed release boots only on its own architecture's node, and this lane is x86_64: $target is ${arch:-unknown}"
[[ -z $board ]] || [[ $(image_dir_for "$arch" "$board") == "release-image-$board" ]] \
  || fail "no release image directory for $target"
fault=${T_INPUT_FAULT:-greetd}
case $fault in greetd | greeter | unlock) ;; *) fail "fault must be greetd, greeter or unlock, not '$fault'" ;; esac

step "tools"
missing=()
for p in systemd-container openssh-clients sshpass socat btrfs-progs dosfstools e2fsprogs \
  mtools util-linux procps-ng jq skopeo fuse-overlayfs qemu-kvm qemu-img edk2-ovmf ipxe-roms-qemu; do
  rpm -q "$p" >/dev/null 2>&1 || missing+=("$p")
done
command -v podman >/dev/null || missing+=(podman)
((${#missing[@]} == 0)) || dnf install -y --setopt=install_weak_deps=False "${missing[@]}" >/dev/null
command -v qemu-system-x86_64 qemu-img ssh sshpass socat podman skopeo systemd-repart >/dev/null
modprobe fuse 2>/dev/null || true
modprobe loop 2>/dev/null || true
"$repo_root/os/dev/assert-native-arch.sh" "$arch"
python3 "$repo_root/os/dev/probe-kvm.py" "$arch" || fail 'this node cannot create a KVM VM; a boot under TCG is too slow to count'

rm -rf "$work" "$out"
mkdir -p "$work" "$out" "$scratch"

# --- the release image, as this commit built it ------------------------------
step "the sealed release image"
[[ -f $image/oci/index.json && -s $image/digest ]] || fail "$image holds no OS image layout (oci/)"
commit=$(git rev-parse HEAD)
image_id=$(podman pull -q "oci:$image/oci")
[[ $(podman image inspect --format '{{.Digest}}' "$image_id") == "$(cat "$image/digest")" ]] \
  || fail "$image/oci is not the image $image/digest names"
podman tag "$image_id" localhost/tilo:release
[[ $(podman image inspect --format '{{index .Labels "org.opencontainers.image.revision"}}' localhost/tilo:release) == "$commit" ]] \
  || fail "$image is not this tree's build ($commit): the lane boots what this commit released"
[[ $(podman image inspect --format '{{index .Labels "org.tilo.rust-lto"}}' localhost/tilo:release) == thin ]] \
  || fail 'a release ships thin-LTO binaries: the image is not os-release-image'
release_digest=$(podman image inspect --format '{{.Digest}}' localhost/tilo:release)
ok "$image is this tree's release image, $release_digest"

# --- installed the way a device installs it -----------------------------------
# The first install seals the UKI with the composefs digest it computes, and a
# sealed disk's /boot is the ESP itself: the layout this lane is about.
step "install"
export TILO_IMAGE=localhost/tilo:release TILO_SKIP_BUILD=1 TILO_DISTROBOX= TILO_VM_ARCH=$arch \
  TILO_OS_FLAVOUR=release TILO_OUTPUT=$work/install TILO_RUN=$work/install-run TMPDIR=/var/tmp
if [[ -n $board ]]; then export TILO_OS_BOARD=$board; fi
bash os/dev/build-image.sh > "$out/install.log" 2>&1 \
  || { tail -n 80 "$out/install.log"; fail 'the release image does not install onto a disk'; }
disk=$work/disk.qcow2
mv "$work/install/qcow2/disk.qcow2" "$disk"
if [[ -n ${TILO_BOOT_ASSESSMENT_DISK:-} ]]; then
  disk=$(cd "$(dirname "$TILO_BOOT_ASSESSMENT_DISK")" && pwd)/$(basename "$TILO_BOOT_ASSESSMENT_DISK")
  [[ -f $disk ]] || fail "no disk at $TILO_BOOT_ASSESSMENT_DISK"
  [[ $fault == unlock ]] \
    || fail 'TILO_BOOT_ASSESSMENT_DISK is for the unlock fault, whose prompt only an encrypted root has'
  ok "installed onto $work/disk.qcow2; the boots below run on $disk, whose root is encrypted"
else
  ok "installed onto $disk"
fi

stage "$stage_name" "$release_digest" || fail "the release does not stage as $stage_reference"

# --- the VM -------------------------------------------------------------------
export TILO_DISTROBOX= TILO_ACCEL=kvm TILO_USER=quickuser TILO_PASS=tilo TILO_VM_ARCH=$arch
export TILO_RUN=$work/run TILO_DISK=$disk TILO_MEM=6144 TILO_SMP=4 TILO_BOOT_WAIT_SECS=420
mkdir -p "$TILO_RUN"
vm=$repo_root/os/dev/vm.sh
guest_scratch=/var/tmp
finish() {
  "$vm" serial > "$out/serial.log" 2>&1 || true
  "$vm" stop >/dev/null 2>&1 || true
}
trap finish EXIT

step "boot the installed release"
"$vm" boot --2d --disk "$disk"
for _ in $(seq 1 210); do "$vm" ssh true >/dev/null 2>&1 && break; sleep 2; done
"$vm" ssh true >/dev/null || fail 'the installed release did not accept SSH'
ok 'the installed release boots and answers'

# The release comes first, and it has to: os/tests/boot_assessment_rollback.sh
# reads the deployment it makes unable to greet from the entry table before it
# makes one, and an empty storage holds no entry to read. Then the update, in the
# release's own names under local storage, signed over the release's own
# repository and digest - the entry a sealed system follows, and the one thing
# about it this node cannot obtain, since nothing here holds the release key.
step "hand the release to the guest"
podman save --format oci-archive -o "$work/release.oci.tar" localhost/tilo:release >/dev/null
archive=$(sha256sum < "$work/release.oci.tar" | cut -d' ' -f1)
scp_to_guest "$work/release.oci.tar"
as_root "test \"\$(sha256sum < $guest_scratch/release.oci.tar | cut -d' ' -f1)\" = $archive" \
  || fail 'the release archive reached the guest as other bytes'
as_root "podman load -q -i $guest_scratch/release.oci.tar" || fail 'the guest does not load the release'
rm -f "$work/release.oci.tar"
[[ $(as_root "podman image inspect --format '{{.Digest}}' $stage_name") == "$release_digest" ]] \
  || fail "the guest does not hold $stage_name as the release $release_digest"
stage "$update_name" "$release_digest" || fail "the guest does not take the release as $update_reference"
as_root "rm -f $guest_scratch/release.oci.tar"
ok "the guest holds $stage_name and $update_name, both the release $release_digest, both signed"

step "prove the fallback on the sealed disk ($fault fault)"
unlock=
[[ -n ${TILO_BOOT_ASSESSMENT_DISK:-} ]] && unlock=yes
RUNNER="$vm ssh" SUDO_PASS="$TILO_PASS" FAULT="$fault" UNLOCK="$unlock" \
  RESET="$vm stop; $vm boot --2d --disk '$disk'" \
  SERIAL="$vm serial" \
  "$repo_root/os/tests/boot_assessment_rollback.sh" "$update_reference" "$out"
