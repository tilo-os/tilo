#!/usr/bin/env bash
# One architecture's signed target policy, exported from a published OS release and verified, or
# published to the generic package the Imager reads it from.
#
#   target-policy.sh export|publish ARCH IMAGE SOURCE_REVISION RELEASE_ID OUT
#
# IMAGE is the exact published multi-architecture index; SOURCE_REVISION the commit it was built
# from; RELEASE_ID the imager-vMAJOR.MINOR.PATCH release the fragment names. `export` writes
# tilo-target-policy-ARCH.tar, target-policy-catalog-ARCH.json and their checksums into OUT;
# `publish` exports the same bundle again and uploads it with the FORGEJO_TOKEN the check's secret
# provides, verifying the public bytes. Neither signs anything: the bundle's signatures come from
# the release authority, and the image's from the release that published it.
set -euo pipefail
repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd -P)"
die() { printf 'target-policy: %s\n' "$*" >&2; exit 1; }

mode=${1:?usage: target-policy.sh export|publish ARCH IMAGE SOURCE_REVISION RELEASE_ID OUT}
arch=${2:?}
image=${3:?}
source_revision=${4:?}
release_id=${5:?}
out=${6:?}
case $mode in export|publish) ;; *) die "unknown mode: $mode" ;; esac
[[ $arch =~ ^(x86_64|aarch64)$ ]] || die "unknown architecture: $arch"
[[ $image =~ ^registry\.tilo\.so/tilo/tilo@sha256:[0-9a-f]{64}$ ]] \
  || die "IMAGE must be a published registry.tilo.so/tilo/tilo digest"
[[ $source_revision =~ ^[0-9a-f]{40}$ ]] || die "SOURCE_REVISION must be a full commit"
[[ $release_id =~ ^imager-v[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "RELEASE_ID must be imager-vX.Y.Z"

tool_dir="$(mktemp -d "${TMPDIR:-/var/tmp}/tilo-target-policy-tools.XXXXXX")"
trap 'rm -rf "$tool_dir"' EXIT
RUNNER_TEMP=$tool_dir GITHUB_PATH=$tool_dir/github-path \
  TILO_GITHUB_ASSET_CACHE=/target/signing-tools TILO_ASSET_OFFLINE=1 \
  bash "$repo/os/dev/install-cached-cosign.sh" >/dev/null
export PATH="$(cat "$tool_dir/github-path"):$PATH"

rm -rf "$out"
python3 "$repo/apps/imager/packaging/export-target-policy.py" export \
  --image "$image" --arch "$arch" --release-id "$release_id" \
  --expected-source "$source_revision" --output "$out"
python3 "$repo/apps/imager/packaging/export-target-policy.py" verify \
  --bundle "$out" --release-id "$release_id"
(cd "$out" && sha256sum "tilo-target-policy-$arch.tar" "target-policy-catalog-$arch.json") \
  > "$out/artifacts.sha256"
(cd "$out" && sha256sum --check --strict artifacts.sha256 >/dev/null)

if [ "$mode" = publish ]; then
  GITHUB_REF_NAME=$release_id bash "$repo/apps/imager/packaging/publish-target-policy.sh" "$out"
  printf 'published the %s target policy of %s\n' "$arch" "$release_id"
else
  printf 'exported the %s target policy of %s\n' "$arch" "$release_id"
fi
