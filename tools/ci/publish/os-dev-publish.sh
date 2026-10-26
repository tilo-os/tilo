#!/usr/bin/env bash
# Publish main's dev image for one architecture, the dev image with the development key's
# signature sign-dev-os-<arch> wrote beside it, under names production releases never use:
#
#   os-dev-publish.sh image ARCH IMAGE DEV       once os-dev-gate-ARCH passed on it
#   os-dev-publish.sh disk ARCH IMAGE DEV DISK   with its disk, once os-acceptance-ARCH passed
#                                                on the disk os-dev-disk-ARCH installed
#
# IMAGE: os-image-ARCH's layout, DEV: os-dev-image-ARCH's (signed), DISK: os-dev-disk-ARCH's
# output. Both push
#   registry.tilo.so/tilo/tilo-ci:main-<commit7>-ARCH     the OS image the dev image is built FROM
#   registry.tilo.so/tilo/tilo-vm:main-<commit7>-ci-ARCH  the dev image, signed
#   registry.tilo.so/tilo/tilo-vm:main-<commit7>-ci       an index of the architectures published
#                                                        so far
# `image` moves tilo-vm:ARCH-ci, the architecture's dev channel, to it, only forward along main;
# `disk` publishes the disk (os/dev/publish-dev-disk.sh), which `tilo-vm vm create` takes and
# os-dev-gate-ARCH updates from, and moves tilo-vm:latest.
# Logs in to registry.tilo.so with an identity token; the disks go to the generic packages with
# TILO_PACKAGES_TOKEN.
set -euo pipefail
here=$(dirname -- "${BASH_SOURCE[0]}")
. "$here/lib.sh"
cd "$repo_root"
usage='usage: os-dev-publish.sh image ARCH IMAGE DEV | disk ARCH IMAGE DEV DISK'
mode=${1:?$usage}
arch=${2:?$usage}
image=${3:?$usage}
dev=${4:?$usage}
case $mode in
  image) ;;
  disk) disk=${5:?$usage}/disk ;;
  *) die "$usage" ;;
esac
[[ $(channel) == dev ]] || die "dev images publish from main only; this run is for $T_REF"
require_commit
[[ $(cat "$dev/revision") == "$T_COMMIT" ]] || die "$dev holds the dev image of $(cat "$dev/revision"), the run is for $T_COMMIT"
[[ -s $dev/signature ]] || die "$dev holds no signature: the check needs sign-dev-os-$arch"
if [[ $mode == disk ]]; then
  [[ -n ${TILO_PACKAGES_TOKEN:-} ]] || die 'TILO_PACKAGES_TOKEN is required: the check needs secrets = ["TILO_PACKAGES_TOKEN"]'
  [[ $(cat "$disk/revision") == "$T_COMMIT" ]] || die "$disk holds the disk of $(cat "$disk/revision"), the run is for $T_COMMIT"
  [[ $(cat "$disk/image-digest") == "$(cat "$dev/digest")" ]] || die "$disk/disk.qcow2 was not installed from $dev"
  [[ -s $disk/disk.qcow2 ]] || die "no installed disk in $disk"
fi
missing=()
for p in qemu-img jq curl; do rpm -q "$p" >/dev/null 2>&1 || missing+=("$p"); done
((${#missing[@]} == 0)) || dnf install -y --setopt=install_weak_deps=False "${missing[@]}" >/dev/null

auth=$scratch/registry-auth.json
export REGISTRY_AUTH_FILE=$auth DOCKER_CONFIG=$scratch/docker
mkdir -p "$DOCKER_CONFIG"
login() {
  id_token registry.tilo.so | skopeo login --authfile "$auth" -u oidc --password-stdin registry.tilo.so >/dev/null
  cp "$auth" "$DOCKER_CONFIG/config.json"
}
login

tag=main-${T_COMMIT:0:7}
base_digest=$(cat "$image/digest")
registry_copy "$auth" "oci:$image/oci" "docker://registry.tilo.so/tilo/tilo-ci:$tag-$arch"
[[ $(skopeo inspect --raw "docker://registry.tilo.so/tilo/tilo-ci:$tag-$arch" | sha256sum | awk '{print "sha256:" $1}') == "$base_digest" ]] \
  || die "registry.tilo.so/tilo/tilo-ci:$tag-$arch is not $base_digest"

# The dev image, signed, by the digest its build recorded; once gated, its channel tag, only
# forward.
repository=registry.tilo.so/tilo/tilo-vm
tags=("$tag-ci-$arch")
if [[ $mode == image ]]; then
  current=$(skopeo inspect --format '{{index .Labels "org.opencontainers.image.revision"}}' \
    "docker://$repository:$arch-ci" 2>/dev/null) || current=
  if moves_forward "$current"; then tags+=("$arch-ci"); fi
fi
bash "$here/oci-publish.sh" dev "$repository" "$dev" "${tags[@]}"

# The index, and the disk; the identity token lasts minutes and the disk upload outlives it.
login
if [[ $mode == image ]]; then
  PUSHED_DIGEST=$(cat "$dev/digest") DEV_TAG=$tag-ci ARCH=$arch SOURCE_REVISION=$T_COMMIT \
    DISK_TOOLING_REVISION=$T_COMMIT BASE_REPOSITORY=registry.tilo.so/tilo/tilo-ci BASE_TRUST=integration \
    BASE_INDEX_DIGEST=$base_digest BASE_CHILD_DIGEST=$base_digest IMAGE_ONLY=1 \
    bash os/dev/publish-dev-disk.sh
else
  PUSHED_DIGEST=$(cat "$dev/digest") DEV_TAG=$tag-ci ARCH=$arch SOURCE_REVISION=$T_COMMIT \
    DISK_TOOLING_REVISION=$T_COMMIT SOURCE_DISK=$disk/disk.qcow2 SOURCE_BOOT=$disk/direct-boot \
    BASE_REPOSITORY=registry.tilo.so/tilo/tilo-ci BASE_TRUST=integration \
    BASE_INDEX_DIGEST=$base_digest BASE_CHILD_DIGEST=$base_digest CHANNEL=true \
    TILO_PACKAGE_TOKEN=$TILO_PACKAGES_TOKEN TILO_PACKAGE_PUBLIC_BASE_URL=https://code.tilo.so/api/packages/tilo/generic \
    TILO_VM_PUBLISH_DIR=$scratch/tilo-vm-publish bash os/dev/publish-dev-disk.sh
fi
