#!/usr/bin/env bash
# The server image's seal and boot signature, the desktop release's own steps (os-release.sh):
#
#   host-seal.sh seal IMAGE OUT        no secrets, as root in a micro-VM step: the image image-host
#                                      built (IMAGE/image.oci.tar) installed onto a scratch disk as a
#                                      server installs it, which rewrites its UKI's placeholder
#                                      composefs digest to the one the install computes
#                                      (os/dev/build-image.sh); the sealed image into OUT
#                                      (image.oci.tar, digest, generation, rpms-source) with what its
#                                      boot signing signs (OUT/boot, os/dev/sign-target-prediction.sh)
#   host-seal.sh apply SEALED SIGNED OUT
#                                      no secrets: the EFI and PCR signatures os-release.sh sign-boot
#                                      left in SIGNED/boot verified against the pinned keys (or, with
#                                      TILO_BOOT_SIGNING_DRY_RUN=1, the throwaway signer's) and added
#                                      under /boot, the image installed once more to prove the seal
#                                      still holds; the signed image into OUT
#
# The sign step between them is os-release.sh sign-boot, the one that holds the keys and runs
# nothing of the image.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"
usage='usage: host-seal.sh seal IMAGE OUT | apply SEALED SIGNED OUT'
name=localhost/tilo-host:seal

# What the install and the measurement run: podman, the disk tools build-image.sh installs with,
# and binutils, which reads the UKI's sections for the PCR 11 measurement.
tools() {
  bash tools/ci/install-packages.sh tools/ci/packages/boot-assessment.txt
  rpm -q binutils systemd-container >/dev/null 2>&1 \
    || dnf install -y --setopt=install_weak_deps=False binutils systemd-container >/dev/null
  modprobe fuse 2>/dev/null || true
  modprobe loop 2>/dev/null || true
  mkdir -p "$scratch/boot-output" "$scratch/boot-run"
}

cmd_seal() {
  local image=$1 out=$2
  tools
  load_archive "$image" "$name"
  rm -rf "$out"
  mkdir -p "$out"
  install_disk "$name" > "$out/install.log" 2>&1 \
    || { tail -n 80 "$out/install.log"; die 'the image does not install onto a disk'; }
  rm -rf "$scratch/boot-output"
  save_archive "$name" "$out"
  [[ $(cat "$out/generation") == "$(cat "$image/generation")" ]] \
    || die "the sealed image carries generation $(cat "$out/generation"), not the $(cat "$image/generation") it was built with"
  cp "$image/rpms-source" "$out/rpms-source"
  bash os/dev/sign-target-prediction.sh prepare "$name" "$out/boot"
  printf 'sealed %s, generation %s\n' "$(cat "$out/digest")" "$(cat "$out/generation")"
}

cmd_apply() {
  local sealed=$1 signed=$2 out=$3 id
  tools
  load_archive "$sealed" "$name"
  rm -rf "$out"
  mkdir -p "$out"
  cp -a "$signed/boot" "$scratch/boot"
  # Only /boot changes, so the composefs digest the UKI seals, which the install checks, holds.
  # The layer is dated as the image is, so the same signatures make the same image.
  efi_cert=
  [[ ${TILO_BOOT_SIGNING_DRY_RUN:-0} == 1 ]] || efi_cert=$(publication TILO_EFI_SIGNING_CERT_SHA256)
  SOURCE_DATE_EPOCH=$(podman image inspect --format '{{.Created.Unix}}' "$name") \
    TILO_EFI_SIGNING_CERT_SHA256=$efi_cert \
    bash os/dev/sign-target-prediction.sh apply "$name" "$scratch/boot"
  id=$(podman image inspect --format '{{.Id}}' "$name")
  install_disk "$name" > "$out/install.log" 2>&1 \
    || { tail -n 80 "$out/install.log"; die 'the signed image does not install'; }
  rm -rf "$scratch/boot-output"
  [[ $(podman image inspect --format '{{.Id}}' "$name") == "$id" ]] \
    || die 'adding the signed /boot files moved the composefs digest the UKI seals'
  save_archive "$name" "$out"
  cp "$sealed/rpms-source" "$out/rpms-source"
  # The provenance a release attests (sign-host), which the server's generation series is read
  # by (os/dev/next-generation.sh): the image, its generation, the base and the snapshot it was
  # built from, the commit left as {commit}. Its kernel is the base's own.
  . os/base-snapshot.env
  RELEASE_DIGEST=$(cat "$out/digest") TILO_GENERATION=$(cat "$out/generation") \
    SOURCE_DATE_EPOCH=$(podman image inspect --format '{{.Created.Unix}}' "$name") \
    BASE_REF=$BASE_REF KERNEL_RPMS_REF=$BASE_REF RPMS_REF=$(rpms_ref_of "$out/rpms-source") \
    SOURCE_REVISION='{commit}' TILO_RELEASE_ARCHITECTURE=x86_64 \
    bash os/dev/release-provenance.sh "$out/provenance.json"
  printf 'signed boot chain on %s, generation %s\n' "$(cat "$out/digest")" "$(cat "$out/generation")"
}

# The snapshot reference a build recorded (snapshot_for_build): the published one, or, for a
# build of a set no release published, its digest, which no release signs (oci-sign.sh refuses it).
rpms_ref_of() {
  local source
  source=$(cat "$1")
  if [[ $source =~ ^registry\.tilo\.so/tilo/rpms@sha256:[0-9a-f]{64}$ ]]; then
    printf '%s' "$source"
  else
    printf 'registry.tilo.so/tilo/rpms@%s' "${source##* }"
  fi
}

case ${1:-} in
  seal) cmd_seal "${2:?$usage}" "${3:?$usage}" ;;
  apply) cmd_apply "${2:?$usage}" "${3:?$usage}" "${4:?$usage}" ;;
  *) die "$usage" ;;
esac
