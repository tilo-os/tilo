#!/usr/bin/env bash
# The inputs of one architecture's WPE engine stage, as a short key:
#
#   os/dev/wpe-stage-key.sh ARCH
#
# build-buildroots.yml labels each published engine stage with it
# (org.tilo.wpe.inputs), and build-image.yml takes a pinned stage only when its
# label equals the key of the checkout; otherwise it compiles the engine, and
# the key addresses that compile's layer cache.
set -euo pipefail
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
case "${1:-}" in
  x86_64) name=WPE_BUILDROOT_X86_64_REF ;;
  aarch64) name=WPE_BUILDROOT_AARCH64_REF ;;
  *) echo "usage: wpe-stage-key.sh x86_64|aarch64" >&2; exit 2 ;;
esac
[[ $# -eq 1 || ( $# -eq 2 && $2 == --legacy ) ]] \
  || { echo "usage: wpe-stage-key.sh x86_64|aarch64 [--legacy]" >&2; exit 2; }
buildroot="$(sed -n "s/^$name=//p" "$repo/os/wpe-buildroot-snapshot.env")"
[[ $buildroot =~ ^registry\.tilo\.so/tilo/tilo-wpe-buildroot@sha256:[0-9a-f]{64}$ ]] \
  || { echo "no pinned WPE buildroot for $1" >&2; exit 1; }
{
  printf '%s\n' "$buildroot"
  cat "$repo/os/Containerfile.wpe" "$repo/os/dev/build-wpe.sh"
  if [[ ${2:-} == --legacy ]]; then
    # Read an existing CI layer cache written before the WPE-only key change.
    cat "$repo/os/external-assets/github-assets.tsv"
  else
    # Only archives copied by Containerfile.wpe belong in the engine key.
    # Fail closed if a listed archive is missing or duplicated.
    for archive in libwpe-1.16.3.tar.xz wpebackend-fdo-1.16.1.tar.xz wpewebkit-2.52.6.tar.xz; do
      rows="$(awk -F '\t' -v name="$archive" '$2 == name {print}' \
        "$repo/os/external-assets/github-assets.tsv")"
      [[ $(printf '%s\n' "$rows" | wc -l) -eq 1 && -n $rows ]] \
        || { echo "expected exactly one manifest row for $archive" >&2; exit 1; }
      printf '%s\n' "$rows"
    done
  fi
} | sha256sum | cut -c1-16
