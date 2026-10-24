#!/usr/bin/env bash
# Publish main's dev VM image and disk for one architecture (publish-os-dev-<arch> in t.toml),
# once os-acceptance-<arch> passed on them: unsigned development artifacts under names
# production policy rejects (os/etc/containers/policy.json).
#
#   os-dev-publish.sh ARCH IMAGE DEV ACCEPTANCE   IMAGE: os-image-ARCH's layout, DEV:
#                                                 os-dev-image-ARCH's, ACCEPTANCE: the gated disk
#
# Pushes
#   registry.tilo.so/tilo/tilo-ci:main-<commit7>-ARCH   the OS image the dev image is built FROM
#   registry.tilo.so/tilo/tilo-vm:main-<commit7>-ci     the dev image, an index of the
#                                                      architectures published so far, and its
#                                                      disk (os/dev/publish-dev-disk.sh), which
#                                                      `tilo-vm vm create` and `vm upgrade --from
#                                                      sha:<commit>` take
# and moves tilo-vm:latest (publish-dev-disk.sh) and, once both architectures are in the index,
# tilo-vm:main-ci to it, each only forward along main. Logs in to registry.tilo.so with an
# identity token; the disks go to the generic packages with TILO_PACKAGES_TOKEN.
set -euo pipefail
here=$(dirname -- "${BASH_SOURCE[0]}")
. "$here/lib.sh"
cd "$repo_root"
arch=${1:?usage: os-dev-publish.sh ARCH IMAGE DEV ACCEPTANCE}
image=${2:?}
dev=${3:?}
disk=${4:?}/disk
[[ $(channel) == dev ]] || die "dev VM images publish from main only; this run is for $T_REF"
require_commit
[[ -n ${TILO_PACKAGES_TOKEN:-} ]] || die 'TILO_PACKAGES_TOKEN is required: the check needs secrets = ["TILO_PACKAGES_TOKEN"]'
[[ $(cat "$dev/revision") == "$T_COMMIT" ]] || die "$dev holds the dev image of $(cat "$dev/revision"), the run is for $T_COMMIT"
[[ $(cat "$disk/revision") == "$T_COMMIT" ]] || die "$disk holds the disk of $(cat "$disk/revision"), the run is for $T_COMMIT"
[[ $(cat "$disk/image-digest") == "$(cat "$dev/digest")" ]] || die "$disk/disk.qcow2 was not installed from $dev"
[[ -s $disk/disk.qcow2 ]] || die "no gated disk in $disk"
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
skopeo copy --preserve-digests --retry-times 3 "oci:$image/oci" "docker://registry.tilo.so/tilo/tilo-ci:$tag-$arch"
[[ $(skopeo inspect --raw "docker://registry.tilo.so/tilo/tilo-ci:$tag-$arch" | sha256sum | awk '{print "sha256:" $1}') == "$base_digest" ]] \
  || die "registry.tilo.so/tilo/tilo-ci:$tag-$arch is not $base_digest"

id=$(podman pull -q "oci:$dev/oci")
podman tag "$id" localhost/tilo-vm:ci
# The identity token lasts minutes and the disk upload outlives it; the image pushes come first.
login
DEV_IMAGE=localhost/tilo-vm:ci DEV_TAG=$tag-ci ARCH=$arch SOURCE_REVISION=$T_COMMIT \
  DISK_TOOLING_REVISION=$T_COMMIT SOURCE_DISK=$disk/disk.qcow2 SOURCE_BOOT=$disk/direct-boot \
  BASE_REPOSITORY=registry.tilo.so/tilo/tilo-ci BASE_TRUST=integration \
  BASE_INDEX_DIGEST=$base_digest BASE_CHILD_DIGEST=$base_digest CHANNEL=true \
  TILO_PACKAGE_TOKEN=$TILO_PACKAGES_TOKEN TILO_PACKAGE_PUBLIC_BASE_URL=https://code.tilo.so/api/packages/tilo/generic \
  TILO_VM_PUBLISH_DIR=$scratch/tilo-vm-publish bash os/dev/publish-dev-disk.sh

# tilo-vm:main-ci follows main's dev images of both architectures: the origin a machine installed
# from a main-ci disk upgrades from. Whichever architecture completes the index moves it.
repository=registry.tilo.so/tilo/tilo-vm
raw=$(skopeo inspect --raw "docker://$repository:$tag-ci")
jq -e '[.manifests[].platform.architecture] | sort == ["amd64","arm64"]' <<< "$raw" >/dev/null || {
  echo "$repository:$tag-ci holds one architecture so far; main-ci stays"
  exit 0
}
digest=sha256:$(printf '%s' "$raw" | sha256sum | awk '{print $1}')
current=$(skopeo inspect --override-arch amd64 "docker://$repository:main-ci" 2>/dev/null \
  | jq -r '.Labels["org.opencontainers.image.revision"] // empty') || current=
if [[ -n $current && $current != "$T_COMMIT" ]]; then
  ahead=$(curl -fsS "https://code.tilo.so/api/v1/repos/tilo/tilo/compare/$T_COMMIT...$current" \
    | sed -n 's/.*"total_commits":\([0-9]*\).*/\1/p') || ahead=
  if [[ $ahead != 0 ]]; then
    echo "$repository:main-ci names $current, which is not an ancestor of $T_COMMIT; leaving it"
    exit 0
  fi
fi
skopeo copy --multi-arch index-only --preserve-digests "docker://$repository@$digest" "docker://$repository:main-ci"
echo "$repository:main-ci -> $tag-ci ($digest)"
