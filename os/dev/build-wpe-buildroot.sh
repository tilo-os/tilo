#!/usr/bin/env bash
# Explicit local bootstrap for the digest-pinned WPE buildroot.
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
os_dir="$(cd -- "$script_dir/.." && pwd)"
repo="$(cd -- "$os_dir/.." && pwd)"
# shellcheck disable=SC1091
. "$os_dir/base-snapshot.env"
: "${BASE_REF:?missing BASE_REF in os/base-snapshot.env}"

arch="$(uname -m)"
case "$arch" in x86_64|aarch64) ;; *) printf 'unsupported WPE buildroot architecture: %s\n' "$arch" >&2; exit 1 ;; esac
tag="${QOS_WPE_BUILDROOT_TAG:-localhost/qos-wpe-buildroot:bootstrap-$arch}"
printf '[qos] explicitly bootstrapping WPE buildroot from Fedora repositories\n' >&2
podman build --file "$os_dir/Containerfile.wpe-buildroot" \
  --build-arg "BASE_REF=$BASE_REF" --tag "$tag" "$repo" >&2
printf '%s\n' "$tag"
