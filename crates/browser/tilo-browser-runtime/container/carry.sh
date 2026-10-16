#!/usr/bin/env bash
# Derive the image to deploy from a freshly built web entry image (INCOMING)
# and the image it replaces (OUTGOING): INCOMING plus OUTGOING's own
# v/<version>/ (Containerfile.carry), tagged TAG. A tab still on the outgoing
# version can then open an app it has not loaded yet without a reload.
#
# Only the outgoing image's own version is carried, the one its version.json
# names, never a version it carried itself; and INCOMING must serve exactly
# its own version. So an image holds at most two versions, and a tab survives
# one deploy, not two. When both images hold the same version there is
# nothing to carry and TAG is INCOMING.
#
# Both images must be local. Prints the carried version, or nothing.
# usage: carry.sh INCOMING OUTGOING TAG   (docker or podman as $CONTAINER, default docker)
# With docker, the build uses the builder in $BUILDX_BUILDER: a docker-driver
# builder (the daemon's own, `default`) sees local images, a docker-container
# one does not.
set -euo pipefail
incoming=${1:?usage: carry.sh INCOMING OUTGOING TAG}
outgoing=${2:?usage: carry.sh INCOMING OUTGOING TAG}
tag=${3:?usage: carry.sh INCOMING OUTGOING TAG}
engine=${CONTAINER:-docker}
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
fail() { echo "carry: $*" >&2; exit 1; }

# IMAGE COMMAND…: run a command in the image instead of the server.
in_image() { local image=$1; shift; "$engine" run --rm --network none --entrypoint "$1" "$image" "${@:2}"; }
version_of() {
  local version
  version=$(in_image "$1" cat /srv/tilo-web/version.json | jq -r .version)
  [[ $version =~ ^[0-9a-f]{20}$ ]] || fail "$1: version.json names no version: $version"
  echo "$version"
}
versions_in() { in_image "$1" ls /srv/tilo-web/v | sort; }

new=$(version_of "$incoming")
[[ $(versions_in "$incoming") == "$new" ]] \
  || fail "$incoming must serve only its own version $new, not: $(versions_in "$incoming" | tr '\n' ' ')"
old=$(version_of "$outgoing")
in_image "$outgoing" test -f "/srv/tilo-web/v/$old/build.json" \
  || fail "$outgoing does not hold its own version $old"

if [[ $old == "$new" ]]; then
  echo "carry: $outgoing serves $new as well; nothing to carry" >&2
  "$engine" tag "$incoming" "$tag"
  exit 0
fi
platform_of() { "$engine" image inspect --format '{{.Os}}/{{.Architecture}}' "$1"; }
platform=$(platform_of "$incoming")
[[ $(platform_of "$outgoing") == "$platform" ]] || fail "$outgoing is $(platform_of "$outgoing"), $incoming is $platform"
context=$(mktemp -d)
trap 'rm -rf "$context"' EXIT
"$engine" build --quiet --pull=false --platform "$platform" -f "$here/Containerfile.carry" \
  --build-arg INCOMING="$incoming" --build-arg OUTGOING="$outgoing" --build-arg OUTGOING_VERSION="$old" \
  -t "$tag" "$context" >/dev/null
[[ $(versions_in "$tag") == "$(printf '%s\n' "$new" "$old" | sort)" ]] \
  || fail "$tag serves $(versions_in "$tag" | tr '\n' ' '), not $new and $old"
echo "$old"
