#!/usr/bin/env bash
# The build half of the frozen RPM snapshot (os/Containerfile.rpms) for this node's
# architecture: every package the OS Containerfiles name, resolved from Fedora, signature-checked
# and laid out as a repository image, into OUT (oci-build.sh). Holds no secrets.
#
# usage: rpms-build.sh ARCH OUT
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
arch=${1:?usage: rpms-build.sh ARCH OUT}
out=${2:?}
case $arch in
  x86_64) platform=linux/amd64 ;;
  aarch64) platform=linux/arm64 ;;
  *) die "unknown architecture $arch" ;;
esac
[[ $(uname -m) == "$arch" ]] || die "the $arch snapshot is built natively, not on $(uname -m)"
cd "$repo_root"
. os/base-snapshot.env
packages=$(python3 os/dev/derive-rpm-package-set.py --arch "$arch" | tr '\n' ' ')
[[ -n ${packages// /} ]] || die "no packages for $arch"
assets=$scratch/github-assets
TILO_GITHUB_ASSET_CACHE=$assets TILO_ASSET_STORE= bash os/dev/cache-github-assets.sh \
  virtio-win-0.1.302-1.noarch.rpm
bash tools/ci/publish/oci-build.sh . os/Containerfile.rpms "$out" --platform "$platform" \
  --build-context "github-assets=$assets" \
  --build-arg "BASE_REF=$BASE_REF" --build-arg "PACKAGES=$packages"
