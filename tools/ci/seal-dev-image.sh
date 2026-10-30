#!/usr/bin/env bash
# A dev OS image, sealed as a release is: its UKI carries the composefs digest `bootc install`
# computes from its layers, not the placeholder a dev build seals. An installed machine's bootc
# switches only to a sealed image; signin-e2e's rollback variant switches to the image from
# before pam_tilo, which a dev build of that commit leaves unsealed.
#
#   seal-dev-image.sh IN OUT     IN and OUT are OCI archives; OUT is IN with only its UKI
#                                (and the target UKI, where it has one) resealed
#
# Rootless podman is enough: the digest is computed by the resealing bootc
# (os/Containerfile.bootc-helper) from the archive, in a container of the pinned Fedora bootc
# base, and os/dev/reseal-image.sh adds the /boot-only layer. A sealed image comes out with the
# same digest, as it leaves /boot out.
set -euo pipefail
in=$(realpath "${1:?usage: seal-dev-image.sh IN OUT}")
out=$(realpath -m "${2:?usage: seal-dev-image.sh IN OUT}")
cd "$(dirname -- "${BASH_SOURCE[0]}")/../.."
. tools/ci/publish/build-lib.sh

base=$(. os/base-snapshot.env && printf '%s' "${BASE_REF:-}")
[[ $base =~ ^registry\.tilo\.so/tilo/fedora-bootc@sha256:[0-9a-f]{64}$ ]] \
  || die 'os/base-snapshot.env must pin the Fedora tooling base by digest'
helper=$(bootc_helper)
work=$(mktemp -d "${TMPDIR:-/var/tmp}/seal-dev-image.XXXXXX")
name=localhost/tilo-seal-dev-image:$(basename "$work" | tr 'A-Z' 'a-z' | tr -c 'a-z0-9\n' -)
trap 'podman rmi -f "$name" >/dev/null 2>&1 || true; rm -rf "$work"' EXIT
mkdir "$work/repo"

digest=$(podman run --rm --network=none --security-opt label=disable \
  --mount "type=image,source=$helper,destination=/helper" \
  -v "$in:/in.ociarchive:ro" -v "$work/repo:/repo" -e BOOTC_COMPOSEFS_DIGEST_REPO=/repo \
  "$base" /helper/bootc container compute-composefs-digest-from-storage oci-archive:/in.ociarchive)
[[ $digest =~ ^[0-9a-f]{128}$ && $digest != "$(printf '0%.0s' $(seq 128))" ]] \
  || die "no composefs digest computed for $in"
printf 'seal-dev-image: composefs digest %s\n' "$digest"

id=$(podman pull -q "oci-archive:$in")
podman tag "$id" "$name"
bash os/dev/reseal-image.sh "$name" "$digest"
# Only /boot may differ from IN: the digest was computed from IN's layers.
before=$(podman image inspect --format '{{json .RootFS.Layers}}' "$id")
after=$(podman image inspect --format '{{json .RootFS.Layers}}' "$name")
[[ ${after%,*} == "${before%]}" ]] || die 'resealing changed more than one added layer'
rm -f "$out"
podman push -q "$name" "oci-archive:$out"
printf 'seal-dev-image: %s sealed into %s (%s)\n' "$in" "$out" "$(archive_digest "$out")"
