#!/usr/bin/env bash
# CI entry point for the USB-free installation VM round trip, run by the
# installer lane of .forgejo/workflows/release-imager.yml.
#
#   ./os/dev/ci-internal-install-cycle.sh INSTALLER_DISK OUT_DIR
#
# Environment (all required unless noted):
#   FEDORA_URL, FEDORA_SHA256        pinned Fedora Cloud Base x86_64 UEFI qcow2
#   TILO_INTERNAL_CYCLE_RELEASE_TAG  imager-vMAJOR.MINOR.PATCH for the test bundle
#   SCENARIO                         all (default) or one scenario name
#   TILO_INTERNAL_CYCLE_STORAGE_ROOT VM storage root (default /var/tmp/...)
#   TILO_INTERNAL_CYCLE_RUN_ID       unique per run (default: $$)
#
# Steps: export the bundle from INSTALLER_DISK, build the test imager whose
# compiled-in catalog names it (os/dev/build-internal-cycle-imager.sh), then
# run each scenario of os/dev/test-internal-install-cycle.sh into
# OUT_DIR/<scenario>. Every scenario runs; any failure fails the job.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
die() { printf 'ci-internal-install-cycle: %s\n' "$*" >&2; exit 1; }
[ "$#" -eq 2 ] || { sed -n '2,19p' "$0" >&2; exit 2; }
disk="$1" out="$2"
[ -f "$disk" ] || die "installer disk not found: $disk"
[[ "${FEDORA_URL:-}" == https://* ]] || die "FEDORA_URL must pin the Fedora Cloud Base image (https)"
[[ "${FEDORA_SHA256:-}" =~ ^[0-9a-f]{64}$ ]] || die "FEDORA_SHA256 must pin the Fedora Cloud Base image"
tag="${TILO_INTERNAL_CYCLE_RELEASE_TAG:?set TILO_INTERNAL_CYCLE_RELEASE_TAG}"
scenario="${SCENARIO:-all}"
run_id="${TILO_INTERNAL_CYCLE_RUN_ID:-$$}"
storage_root="${TILO_INTERNAL_CYCLE_STORAGE_ROOT:-/var/tmp/tilo-internal-cycle-$run_id}"
case "$scenario" in
  all) scenarios=(boot-next-busy remove interrupt-resume round-trip) ;;
  round-trip|interrupt-resume|remove|boot-next-busy) scenarios=("$scenario") ;;
  *) die "unknown scenario: $scenario" ;;
esac

mkdir -p "$out" "$storage_root"
export_dir="$out/export"
rm -rf "$export_dir"
# The multi-gigabyte component images are inputs, not evidence: never leave
# them in the uploaded directory, whatever fails.
trap 'rm -f -- "$export_dir"/bundle/*.img' EXIT
"$repo/os/dev/export-internal-bundle.sh" "$disk" x86_64 "$tag" "$export_dir"
imager="$storage_root/imager"
rm -f "$imager"
"$repo/os/dev/build-internal-cycle-imager.sh" "$export_dir" "$tag" "$imager"
git -C "$repo" diff --exit-code -- apps/imager/release-catalog.json \
  || die "the tracked release catalog was not restored after the test build"

failed=()
port="${TILO_INTERNAL_CYCLE_FIRST_SSH_PORT:-22220}"
for name in "${scenarios[@]}"; do
  port=$((port + 1))
  if ! TILO_VM_CYCLE="internal-$run_id-$name" TILO_SSH_PORT="$port" \
      TILO_INTERNAL_CYCLE_STORAGE="$storage_root/$name" \
      "$repo/os/dev/test-internal-install-cycle.sh" \
        --fedora-url "$FEDORA_URL" --fedora-sha256 "$FEDORA_SHA256" \
        --bundle-dir "$export_dir" --imager "$imager" --release-tag "$tag" \
        --scenario "$name" --out "$out/$name"; then
    failed+=("$name")
  fi
done
[ "${#failed[@]}" -eq 0 ] || die "failed scenarios: ${failed[*]}"
printf 'internal install cycle passed: %s\n' "${scenarios[*]}"
