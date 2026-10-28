#!/usr/bin/env bash
# The OS release (the os-release family in tools/ci/products.toml): the release image
# os-release-image-ARCH composed (os/image/build-layer.sh, TILO_OS_FLAVOUR=release), sealed and
# checked, its boot artifacts signed with the EFI and PCR keys in KMS, signed with the release key,
# attested and published as registry.tilo.so/tilo/tilo:gen<N>, <version> and latest. A board of
# release_boards in os/image/layers.toml has a release of its own, os-release-image-BOARD
# (TILO_OS_BOARD): the same steps, with the board for ARCH, published to code.tilo.so/tilo/tilo,
# the repository the board's update origin names, as <board>-<commit>, <board>-gen<N> in the
# board's own series (os/dev/board-generation.sh) and <board>-latest.
#
#   os-release.sh seal TARGET IMAGE OUT      no secrets, as root in a micro-VM step with KVM: the
#                                            image IMAGE/oci checked inside, installed onto a disk
#                                            as a device installs it (which seals its UKI with the
#                                            composefs digest the install computes) and booted
#                                            until the greeter accepts input with no unit failed
#                                            (a board's is not booted: see cmd_seal); the sealed
#                                            image into OUT (image.oci.tar, digest, generation)
#                                            with what its boot signing signs (OUT/boot)
#   os-release.sh sign-boot SEALED OUT       the EFI and PCR signatures of SEALED/boot, into
#                                            OUT/boot, with the keys in KMS (tools/signing/
#                                            boot-signer.sh); runs nothing of the image
#   os-release.sh apply TARGET SEALED SIGNED OUT
#                                            no secrets: the signatures verified against the
#                                            pinned keys and added under /boot, the image installed
#                                            once more to prove the seal still holds, then
#                                            OUT/image.oci.tar, digest and generation, its SPDX
#                                            SBOM (sbom.spdx.json) and SLSA provenance
#                                            (provenance.json, the commit left as {commit}); a
#                                            board's binds its hardware definition and firmware
#   os-release.sh index DIR                  the index of DIR/x86_64 and DIR/aarch64 into DIR/index,
#                                            with its provenance
#   os-release.sh sign DIR                   with the release key in KMS: each image and the
#                                            index signed, their provenance and a freshness
#                                            statement attested (os/dev/release-freshness.sh)
#   os-release.sh publish DIR                the images as gen<N>-ARCH, the index as gen<N>, on a
#                                            final version tag <version>, and latest, each gen
#                                            tag once
#   os-release.sh sign-board BOARD DIR       the board's image in DIR signed, attested and stated
#                                            fresh as `sign` does an architecture's
#   os-release.sh publish-board BOARD DIR    the board's image as <board>-<commit> and
#                                            <board>-gen<N>, each once, then <board>-latest; an
#                                            empty series gets its seed as gen1 first
#   os-release.sh reproduce TARGET COMPILED OUT
#                                            no secrets: the release image built again, every
#                                            action in this one step from the tree COMPILED (an
#                                            independent compile), and compared with the one
#                                            os-release-image-ARCH composed; a difference fails,
#                                            with a report of it in OUT
#   os-release.sh rehearse TARGET SEALED OUT sign-boot, apply, sign and publish with throwaway
#                                            keys into the OCI layout OUT/registry, as a release of
#                                            this tree from stable would, with no credentials
#
# TARGET is an architecture, x86_64 or aarch64, or a board of release_boards.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"
repository=registry.tilo.so/tilo/tilo
board=

# TARGET (above) as $arch, and for a board $board and its repository.
target() {
  case $1 in
    x86_64 | aarch64) arch=$1 ;;
    *)
      grep -Eq "^release_boards = \[.*\"$1\"" os/image/layers.toml \
        || die "$1 is neither an architecture nor among release_boards in os/image/layers.toml"
      board=$1
      arch=$(sed -n 's/^ *"architecture": *"\([^"]*\)".*/\1/p' "os/hardware/profiles/$board.json")
      repository=code.tilo.so/tilo/tilo
      export TILO_OS_REPOSITORY=$repository ;;
  esac
}

# The release image's directory under /target/os/ARCH.
image_dir() {
  printf 'release-image%s' "${board:+-$board}"
}

oci_arch() {
  case $1 in
    x86_64) echo amd64 ;;
    aarch64) echo arm64 ;;
    *) die "unknown architecture $1" ;;
  esac
}

# Root's tools in the guest: podman, what build-image.sh installs with, and QEMU with $@.
guest_tools() {
  local p missing=()
  for p in dosfstools btrfs-progs systemd-container binutils socat qemu-img jq "$@"; do
    rpm -q "$p" >/dev/null 2>&1 || missing+=("$p")
  done
  command -v podman >/dev/null || missing+=(podman skopeo fuse-overlayfs e2fsprogs util-linux python3)
  ((${#missing[@]} == 0)) || dnf install -y --setopt=install_weak_deps=False "${missing[@]}" >/dev/null
  modprobe fuse 2>/dev/null || true
  modprobe loop 2>/dev/null || true
  use_cosign
}

# The image in the archive DIR/image.oci.tar, checked against DIR/digest, as podman's NAME.
load_archive() {
  local dir=$1 name=$2
  [[ $(archive_digest "$dir/image.oci.tar") == "$(cat "$dir/digest")" ]] || die "$dir/image.oci.tar is not the image $dir/digest names"
  podman tag "$(podman pull -q "oci-archive:$dir/image.oci.tar")" "$name"
}

# podman's NAME into OUT/image.oci.tar, with OUT/digest and OUT/generation.
save_archive() {
  local name=$1 out=$2
  rm -f "$out/image.oci.tar"
  podman push -q --compression-format zstd --compression-level 3 "$name" "oci-archive:$out/image.oci.tar"
  archive_digest "$out/image.oci.tar" > "$out/digest"
  podman run --rm --entrypoint /bin/cat "$name" /usr/lib/tilo/generation > "$out/generation"
  [[ $(cat "$out/generation") =~ ^[1-9][0-9]*$ ]] || die "$name carries no generation"
}

# Install podman's NAME onto a scratch disk as a device installs it: the first install seals its
# UKI with the composefs digest it computes, rewriting NAME (os/dev/build-image.sh).
install_disk() {
  TILO_IMAGE=$1 TILO_SKIP_BUILD=1 TILO_DISTROBOX= TILO_VM_ARCH=$arch \
    TILO_OUTPUT=$scratch/boot-output TILO_RUN=$scratch/boot-run TMPDIR=/var/tmp \
    bash os/dev/build-image.sh
}

cmd_seal() {
  local image=$1 out=$2 status=0 qemu=()
  case $arch in
    x86_64) qemu=(edk2-ovmf qemu-system-x86-core qemu-device-display-virtio-vga) ;;
    aarch64) qemu=(edk2-aarch64 qemu-system-aarch64-core qemu-device-display-virtio-gpu-pci) ;;
  esac
  [[ -f $image/oci/index.json ]] || die "$image holds no OS image layout (oci/)"
  if [[ -n $board ]]; then
    qemu=()
  else
    [[ -c /dev/kvm ]] || die 'no /dev/kvm: the check needs requires = ["/dev/kvm"]'
    qemu+=(ipxe-roms-qemu)
  fi
  guest_tools "${qemu[@]}"
  local id
  id=$(podman pull -q "oci:$image/oci")
  [[ $(podman image inspect --format '{{.Digest}}' "$id") == "$(cat "$image/digest")" ]] || die "$image/oci is not the image $image/digest names"
  podman tag "$id" localhost/tilo:release
  [[ $(podman image inspect --format '{{index .Labels "org.tilo.rust-lto"}}' localhost/tilo:release) == thin ]] \
    || die 'a release ships thin-LTO binaries: the image is not os-release-image'
  rm -rf "$out"
  mkdir -p "$out" "$scratch/boot-output" "$scratch/boot-run"

  # The finished tree, inside the image and without a network.
  podman run --rm --network=none --security-opt label=disable \
    -v "$repo_root/tools/ci/image_smoke.py:/run/image-smoke.py:ro" localhost/tilo:release \
    python3 -I -B /run/image-smoke.py 2>&1 | tee "$out/image-smoke.log" || status=1

  if [[ -n $board ]]; then
    seal_board "$out" || status=1
    ((status == 0)) || exit "$status"
    save_archive localhost/tilo:release "$out"
    bash os/dev/sign-target-prediction.sh prepare localhost/tilo:release "$out/boot"
    return
  fi

  # Installed and booted with the repository's VM harness (os/dev/vm.sh) until the greeter
  # accepts input on its console channel. A unit that fails on the way prints `[FAILED]` on the
  # serial console, where the sealed command line sends it; any such line fails the boot.
  boot() {
    install_disk localhost/tilo:release || { echo 'FAIL  the image does not install onto a disk'; return 1; }
    export TILO_DISK=$scratch/boot-output/qcow2/disk.qcow2 TILO_RUN=$scratch/boot-run TILO_VM_ARCH=$arch \
      TILO_MEM=6144 TILO_SMP=4 TILO_BOOT_WAIT_SECS=420 TILO_DISTROBOX=
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
  ((status == 0)) || exit "$status"
  save_archive localhost/tilo:release "$out"
  bash os/dev/sign-target-prediction.sh prepare localhost/tilo:release "$out/boot"
}

# A board's image, as a board gets it: its profile and update origin, the Lenovo's sound stack, and
# installed onto a disk, which seals its UKI. It is not booted: its UKI carries the board's device
# tree, which systemd-stub hands the kernel in place of the one QEMU's virt machine describes
# itself with, so the kernel would look for the board's controllers, console and storage and
# find none of QEMU's. It boots only on the board.
seal_board() {
  local out=$1 origin
  origin=code.tilo.so/tilo/tilo:$board-$(git rev-parse --verify HEAD) || die 'the seal names its commit: git = true'
  podman run --rm --network=none --entrypoint /bin/cat localhost/tilo:release /usr/lib/tilo/hardware-profile.json \
    | jq -e --arg board "$board" --arg arch "$arch" --arg origin "$origin" \
      '.id == $board and .architecture == $arch and .target_update_ref == $origin' >/dev/null \
    || { echo "FAIL  the image is not $board's release of this commit, following $origin"; return 1; }
  echo "ok    the image is $board's, following $origin"
  if [[ $board == lenovo-83ed ]]; then
    podman run --rm --network=none --entrypoint bash \
      -v "$repo_root/os/tests/board_audio_contract.sh:/run/board_audio_contract.sh:ro" \
      localhost/tilo:release /run/board_audio_contract.sh --root / 2>&1 | tee "$out/audio.log" \
      || { echo 'FAIL  the sound stack is incomplete'; return 1; }
  fi
  install_disk localhost/tilo:release > "$out/install.log" 2>&1 \
    || { tail -n 80 "$out/install.log"; echo 'FAIL  the image does not install onto a disk'; return 1; }
  echo 'ok    the image installs onto a disk, which sealed its UKI'
}

# jsign (the pinned jar, os/external-assets) on PATH, for tools/signing/boot-signer.sh, with the
# AWS CLI it signs the PCR digest with.
use_signing_tools() {
  local jar
  rpm -q java-25-openjdk-headless awscli2 openssl >/dev/null 2>&1 \
    || dnf install -y --setopt=install_weak_deps=False java-25-openjdk-headless awscli2 openssl >/dev/null
  jar=$(pinned_assets jsign-7.6.jar)/jsign-7.6.jar
  mkdir -p "$scratch/bin"
  printf '#!/bin/sh\nexec java -jar %s "$@"\n' "$jar" > "$scratch/bin/jsign"
  chmod 0755 "$scratch/bin/jsign"
  PATH=$scratch/bin:$PATH
}

cmd_sign_boot() {
  local sealed=$1 out=$2
  [[ -s $sealed/boot/message.bin ]] || die "$sealed/boot holds no boot signing request"
  use_signing_tools
  rm -rf "$out"
  mkdir -p "$out"
  cp -a "$sealed/boot" "$out/boot"
  if [[ -z ${TILO_BOOT_SIGNER_DRY_RUN:-} ]]; then
    [[ $(channel) == release ]] || die "only releases sign their boot; this run is for $T_REF"
    [[ ${TILO_EFI_SIGNING_KMS_KEY:-} =~ ^arn:aws:kms:([a-z0-9-]+): ]] || die 'TILO_EFI_SIGNING_KMS_KEY is not a KMS ARN'
    aws_role "${AWS_SIGNING_ROLE_ARN:?the AWS role to sign as}" "${BASH_REMATCH[1]}"
  fi
  bash os/dev/sign-target-prediction.sh sign "$out/boot" "$repo_root/tools/signing/boot-signer.sh"
}

# The provenance predicate of the image in DIR (os/dev/release-provenance.sh), the commit left as
# {commit} for oci-attest.sh, with the pins the release was built from; $@ adds to its environment.
provenance() {
  local dir=$1
  shift
  . os/base-snapshot.env
  . os/kernel-snapshot.env
  local rpms
  rpms=$(bash os/dev/resolve-rpm-snapshot.sh) || die 'a release installs from a published RPM snapshot'
  env RELEASE_DIGEST="$(cat "$dir/digest")" TILO_GENERATION="$(cat "$dir/generation")" \
    SOURCE_DATE_EPOCH="$(bash os/dev/image-epoch.sh "$BASE_REF" "$rpms")" BASE_REF="$BASE_REF" \
    RPMS_REF="$rpms" SOURCE_REVISION='{commit}' "$@" \
    bash os/dev/release-provenance.sh "$dir/provenance.json"
}

cmd_apply() {
  local sealed=$1 signed=$2 out=$3 id
  guest_tools
  load_archive "$sealed" localhost/tilo:release
  # The seal checks an image of a package set stable has yet to publish; a release ships none.
  [[ $(podman image inspect --format '{{index .Labels "org.tilo.rpms.trust"}}' localhost/tilo:release) != unsigned ]] \
    || die 'a release installs from a published RPM snapshot, not the unsigned set of its run'
  rm -rf "$out"
  mkdir -p "$out" "$scratch/boot-output" "$scratch/boot-run"
  cp -a "$signed/boot" "$scratch/boot"
  # Only /boot changes: the composefs digest the UKI seals, which the install checks, holds. The
  # layer is dated as the image is, so the same signatures make the same image.
  SOURCE_DATE_EPOCH=$(podman image inspect --format '{{.Created.Unix}}' localhost/tilo:release) \
    TILO_EFI_SIGNING_CERT_SHA256=$(publication TILO_EFI_SIGNING_CERT_SHA256) \
    bash os/dev/sign-target-prediction.sh apply localhost/tilo:release "$scratch/boot"
  id=$(podman image inspect --format '{{.Id}}' localhost/tilo:release)
  install_disk localhost/tilo:release > "$out/install.log" 2>&1 \
    || { tail -n 80 "$out/install.log"; die 'the signed image does not install'; }
  [[ $(podman image inspect --format '{{.Id}}' localhost/tilo:release) == "$id" ]] \
    || die 'adding the signed /boot sidecars moved the composefs digest the UKI seals'
  save_archive localhost/tilo:release "$out"

  # The complete SPDX inventory, kept beside the image: Rekor's 100 KB attestation cap is smaller,
  # so the provenance names its digest. Syft gives a scan a new timestamp and document namespace
  # each run; both are fixed to the image's, so the same image always has the same SBOM.
  local syft=$scratch/syft
  RUNNER_TEMP=$syft GITHUB_PATH=$syft/path TILO_ASSET_OFFLINE=1 bash os/dev/install-cached-syft.sh >/dev/null
  "$syft/tilo-syft-tools/syft" -q "oci-archive:$out/image.oci.tar" --output "spdx-json=$scratch/sbom.json"
  local digest epoch
  digest=$(cat "$out/digest")
  epoch=$(podman image inspect --format '{{.Created.Unix}}' localhost/tilo:release)
  jq -e --arg digest "$digest" --arg created "$(date -u -d "@$epoch" +%Y-%m-%dT%H:%M:%SZ)" '
    select(.spdxVersion == "SPDX-2.3" and (.packages | type == "array"))
    | .documentNamespace = ("https://tilo.sh/spdx/os/" + ($digest | sub(":"; "-")))
    | .creationInfo.created = $created' "$scratch/sbom.json" > "$out/sbom.spdx.json"
  board_provenance
  provenance "$out" TILO_SBOM_SHA256="$(sha256sum < "$out/sbom.spdx.json" | cut -d' ' -f1)" \
    TILO_RELEASE_ARCHITECTURE="$arch" KERNEL_RPMS_REF="$(kernel_ref "$arch")" "${board_args[@]}"
  printf 'release image %s: %s, generation %s\n' "${board:-$arch}" "$digest" "$(cat "$out/generation")"
}

# What a board's provenance binds besides (os/dev/release-provenance.sh), as board_args: the
# board, and the firmware its image carries, by digest.
board_provenance() {
  board_args=()
  case $board in
    '') ;;
    lenovo-83ed)
      board_args=(TILO_HARDWARE_PROFILE=lenovo-83ed
        "TILO_FIRMWARE_CANDIDATE=$(publication TILO_LENOVO_FIRMWARE_REF)"
        "TILO_FIRMWARE_MANIFEST_SHA256=$(publication TILO_LENOVO_FIRMWARE_MANIFEST_SHA256)") ;;
    *) die "no release provenance for the board $board" ;;
  esac
}

kernel_ref() {
  . os/kernel-snapshot.env
  case $1 in
    x86_64) printf '%s' "$KERNEL_RPMS_X86_64_REF" ;;
    aarch64) printf '%s' "$KERNEL_RPMS_AARCH64_REF" ;;
  esac
}

cmd_index() {
  local dir=$1 a children=()
  use_cosign
  [[ $(cat "$dir/x86_64/generation") == "$(cat "$dir/aarch64/generation")" ]] \
    || die "the images carry generations $(cat "$dir/x86_64/generation") and $(cat "$dir/aarch64/generation")"
  python3 tools/ci/publish/oci-index.py "$dir/index" "$dir/x86_64/image.oci.tar" "$dir/aarch64/image.oci.tar"
  cp "$dir/x86_64/generation" "$dir/index/generation"
  for a in x86_64 aarch64; do
    children+=("{\"architecture\":\"$a\",\"digest\":\"$(cat "$dir/$a/digest")\"}")
  done
  provenance "$dir/index" TILO_RELEASE_CHILDREN_JSON="[${children[0]},${children[1]}]" \
    TILO_KERNEL_REFS_JSON="[{\"architecture\":\"x86_64\",\"ref\":\"$(kernel_ref x86_64)\"},{\"architecture\":\"aarch64\",\"ref\":\"$(kernel_ref aarch64)\"}]"
}

# Sign the image in DIR, attest its provenance and a freshness statement valid for a day.
sign_one() {
  local dir=$1
  bash tools/ci/publish/oci-sign.sh "$repository" "$dir"
  bash tools/ci/publish/oci-attest.sh "$repository" "$dir"
  RELEASE_DIGEST=$(cat "$dir/digest") TILO_GENERATION=$(cat "$dir/generation") SOURCE_REVISION=$T_COMMIT \
    FRESHNESS_TTL_SECONDS=86400 bash os/dev/release-freshness.sh "$dir/freshness.json"
  bash tools/ci/publish/oci-attest.sh "$repository" "$dir" freshness.json https://tilo.sh/security/release-freshness/v1
}

cmd_sign() {
  local dir=$1 d
  for d in "$dir/x86_64" "$dir/aarch64" "$dir/index"; do sign_one "$d"; done
}

cmd_sign_board() {
  sign_one "$1"
}

# The board's image in DIR under its three tags. A tag held by another image fails it: a series
# generation or a commit's release is published once, and never by another image.
publish_board_tags() {
  local dir=$1 held
  bash tools/ci/publish/oci-publish.sh release "$repository" "$dir" \
    "once:$board-{commit}" "once:$board-gen{generation}" "$board-latest"
  held=$(sed -n 's/.*"digest":"\(sha256:[0-9a-f]*\)".*/\1/p' "$dir/receipt.json")
  [[ $held == "$(cat "$dir/digest")" ]] \
    || die "$repository holds $held under this release's tags, not $(cat "$dir/digest")"
}

cmd_publish_board() {
  local dir=$1
  use_cosign
  REGISTRY_AUTH_FILE=$scratch/board-registry-auth.json
  export REGISTRY_AUTH_FILE
  registry_login "${repository%%/*}" "$REGISTRY_AUTH_FILE"
  bash os/dev/board-generation.sh --retain
  publish_board_tags "$dir"
}

cmd_publish() {
  local dir=$1 a
  # The images before the index naming them; latest last, once every durable name holds it.
  for a in x86_64 aarch64; do
    bash tools/ci/publish/oci-publish.sh release "$repository" "$dir/$a" "once:gen{generation}-$a"
  done
  bash tools/ci/publish/oci-publish.sh release "$repository" "$dir/index" 'once:gen{generation}' 'final:{version}' latest
}

cmd_reproduce() {
  local compiled=$1 out=$2 layer image expected built root=$scratch/os
  expected=$(cat "/target/os/$arch/$(image_dir)/digest")
  rm -rf "$out" "$root"
  mkdir -p "$out" "$root/$arch"
  cp -a "$compiled" "$root/$arch/compiled-release"
  run_layer() { TILO_OS_ROOT=$root TILO_OS_FLAVOUR=release TILO_OS_BOARD=$board bash os/image/build-layer.sh "$@"; }
  run_layer base "$arch" "$root/$arch/base"
  for image in $(python3 -I -c 'import tomllib; print(" ".join(i["name"] for i in tomllib.load(open("os/image/layers.toml","rb"))["image"]))'); do
    run_layer image "$image" "$arch" "$root/$arch/$image"
  done
  for layer in $(python3 -I -c '
import sys, tomllib
board = sys.argv[1]
print(" ".join((f"board-layers/{board}/" if board and l.get("board") else "release-layers/" if l.get("release") else "layers/")
               + l["name"] for l in tomllib.load(open("os/image/layers.toml", "rb"))["layer"]))' "$board"); do
    run_layer layer "${layer##*/}" "$arch" "$root/$arch/$layer"
  done
  run_layer compose "$arch" "$out/image"
  built=$(cat "$out/image/digest")
  if [[ $built == "$expected" ]]; then
    printf 'ok    the independent rebuild of the %s release image is %s, byte for byte\n' "${board:-$arch}" "$built"
    rm -rf "$out/image"
    return
  fi
  printf 'FAIL  the independent rebuild is %s, os-release-image-%s %s\n' "$built" "${board:-$arch}" "$expected"
  python3 os/scripts/oci-image-diff.py "/target/os/$arch/$(image_dir)/oci" "$out/image/oci" > "$out/diff.txt" 2>&1 || true
  rm -rf "$out/image"
  head -n 200 "$out/diff.txt"
  exit 1
}

cmd_rehearse() {
  local sealed=$1 out=$2 dir
  dir=$out/release
  rm -rf "$out"
  mkdir -p "$out/registry"
  export TILO_PUBLISH_DRY_RUN=$out/registry TILO_BOOT_SIGNER_DRY_RUN=$scratch/boot-keys TILO_BOOT_SIGNING_DRY_RUN=1
  export T_REF=stable T_COMMIT
  T_COMMIT=$(git rev-parse --verify HEAD) || die 'the rehearsal names its commit: git = true'
  cmd_sign_boot "$sealed" "$scratch/signed"
  cmd_apply "$sealed" "$scratch/signed" "$dir"
  sign_one "$dir"
  if [[ -n $board ]]; then
    # The series as publishing reads it, and its seed verified where it is empty; moving the
    # registry's tags is the release's alone.
    [[ $(bash os/dev/board-generation.sh) == "$(cat "$dir/generation")" ]] \
      || die "the image carries generation $(cat "$dir/generation"), not the next of the $board series"
    publish_board_tags "$dir"
  else
    bash tools/ci/publish/oci-publish.sh release "$repository" "$dir" "once:gen{generation}-$arch"
  fi
  printf 'rehearsed the %s release into %s\n' "${board:-$arch}" "$out/registry"
}

command=${1:-}
case $command in
  seal | apply | reproduce | rehearse)
    target "${2:?usage: os-release.sh $command TARGET ...}"
    oci_arch "$arch" >/dev/null
    [[ $(uname -m) == "$arch" ]] || die "the $arch release is built natively, not on $(uname -m)"
    shift 2 ;;
  sign-board | publish-board)
    target "${2:?usage: os-release.sh $command BOARD DIR}"
    [[ -n $board ]] || die "$2 is not a board"
    shift 2 ;;
  sign-boot | index | sign | publish) shift ;;
  *) die 'usage: os-release.sh seal|sign-boot|apply|index|sign|publish|sign-board|publish-board|reproduce|rehearse ...' ;;
esac
case $command in
  seal) cmd_seal "${1:?}" "${2:?}" ;;
  sign-boot) cmd_sign_boot "${1:?}" "${2:?}" ;;
  apply) cmd_apply "${1:?}" "${2:?}" "${3:?}" ;;
  index) cmd_index "${1:?}" ;;
  sign) require_commit; cmd_sign "${1:?}" ;;
  publish) cmd_publish "${1:?}" ;;
  sign-board) require_commit; cmd_sign_board "${1:?}" ;;
  publish-board) cmd_publish_board "${1:?}" ;;
  reproduce) cmd_reproduce "${1:?}" "${2:?}" ;;
  rehearse) cmd_rehearse "${1:?}" "${2:?}" ;;
esac
