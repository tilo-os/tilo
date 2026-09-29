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
buildroot="$(sed -n "s/^$name=//p" "$repo/os/wpe-buildroot-snapshot.env")"
[[ $buildroot =~ ^registry\.tilo\.so/tilo/tilo-wpe-buildroot@sha256:[0-9a-f]{64}$ ]] \
  || { echo "no pinned WPE buildroot for $1" >&2; exit 1; }
{ printf '%s\n' "$buildroot"
  cat "$repo/os/Containerfile.wpe" "$repo/os/dev/build-wpe.sh" "$repo/os/external-assets/github-assets.tsv"
} | sha256sum | cut -c1-16
