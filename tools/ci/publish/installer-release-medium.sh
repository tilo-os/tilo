#!/usr/bin/env bash
# The live installer medium a release publishes, from an OS release image this
# tree composed, with its internal bundle and the release payload beside it.
#
#   installer-release-medium.sh ARCH IMAGE_DIR OUT
#
# IMAGE_DIR is an OS image build left: the OCI layout `oci/` and the digest that names it
# (os/image/build-layer.sh compose). The medium itself is tools/ci/publish/installer-medium.sh's,
# the same one the server and development media use, so nothing here rebuilds an OS image, a
# package set or an installer layer.
#
# OUT receives
#   disk.qcow2, disk.qcow2.sha256, disk.qcow2.hardware.json, source.json
#                                    the medium, the media provenance the disk step wrote,
#                                    and the image it was made from
#   tilo-installer-linux-ARCH.img.zst and .sha256, catalog-ARCH.json
#                                    the release payload the Imager downloads and writes,
#                                    with the catalog entry that names its bytes
#   bundle/, internal-catalog-ARCH.json
#                                    the USB-free internal bundle (os/dev/export-internal-bundle.sh)
#                                    and the catalog entry that names its package
#   artifacts.sha256                 every file above
#
# A release identity is TILO_INSTALLER_RELEASE_TAG, TILO_INSTALLER_VERSION and
# TILO_INSTALLER_CHANNEL, as apps/imager/packaging/package-install-medium.sh reads them. The
# default is a rehearsal of the tree's commit: it is a valid payload for every producer and
# verifier, and names no release that could be published.
#
# With TILO_INSTALLER_TARGET_OCI naming an offline target (installer-medium.sh's own variables
# beside it), the medium carries that target and is packaged only as a qcow2: the Windows host
# writes it directly, and the raw payload and the internal bundle would be second copies of the
# same disk.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"

arch=${1:?usage: installer-release-medium.sh ARCH IMAGE_DIR OUT}
in=${2:?}
out=${3:?}
case $arch in x86_64|aarch64) ;; *) die "unknown architecture: $arch" ;; esac
[[ $(uname -m) == "$arch" ]] || die "the $arch medium builds natively, not on $(uname -m)"
[[ -f $in/oci/index.json ]] || die "$in holds no OS image layout (oci/)"
[[ -f $in/digest ]] || die "$in names no image (digest)"
source_digest=$(tr -d '[:space:]' < "$in/digest")
[[ $source_digest =~ ^sha256:[0-9a-f]{64}$ ]] || die "$in/digest does not name an image: $source_digest"

release_tag=${TILO_INSTALLER_RELEASE_TAG:-rehearsal-$T_COMMIT}
version=${TILO_INSTALLER_VERSION:-0.0.0}
channel=${TILO_INSTALLER_CHANNEL:-rehearsal}
[[ $T_COMMIT =~ ^[0-9a-f]{40}$ ]] || die "T_COMMIT is not a full commit: ${T_COMMIT:-unset}"
[[ $release_tag =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$ ]] || die "release tag is not a release identity: $release_tag"

rm -rf "$out"
mkdir -p "$out"

# A medium only this run's own steps and the cycle's guest read: uncompressed, and with the
# Btrfs live root the internal bundle's export shrinks.
export TILO_INSTALLER_ROOTFS=btrfs TILO_QCOW2_COMPRESS=0
started=$SECONDS
bash tools/ci/publish/installer-medium.sh "$arch" "$in" "$out" \
  || die "the $arch installer medium did not build"
printf 'TILO_INSTALLER_MEDIUM_TIMING phase=medium seconds=%s\n' "$((SECONDS - started))"

[[ -s $out/disk.qcow2 ]] || die "no installer medium was written"
[[ -f $out/disk.qcow2.hardware.json ]] || die "the medium carries no media provenance"
medium_sha=$(sha256sum "$out/disk.qcow2" | awk '{print $1}')
[[ $(awk '{print $1}' "$out/disk.qcow2.sha256") == "$medium_sha" ]] \
  || die "the medium's own checksum does not name its bytes"
source_image=$(jq -er '.source_image_digest' "$out/source.json")
[[ $source_image == "$source_digest" ]] \
  || die "source.json names $source_image, not the image the medium was made from"

if [ -n "${TILO_INSTALLER_TARGET_OCI:-}" ]; then
  # A preloaded target's medium is written to a disk by the Windows host; the raw payload and
  # the internal bundle would package the same bytes again for a path that never reads them.
  printf 'offline target medium: the qcow2 is the artifact; no raw payload or internal bundle\n'
else
  started=$SECONDS
  TILO_INSTALLER_RELEASE_TAG=$release_tag TILO_INSTALLER_VERSION=$version \
    TILO_INSTALLER_CHANNEL=$channel \
    bash apps/imager/packaging/package-install-medium.sh "$out/disk.qcow2" "$arch" "$out" \
    || die "the $arch release payload did not package"
  printf 'TILO_INSTALLER_MEDIUM_TIMING phase=package seconds=%s\n' "$((SECONDS - started))"

  archive="tilo-installer-linux-$arch.img.zst"
  catalog="catalog-$arch.json"
  [[ -s $out/$archive && -s $out/$archive.sha256 && -s $out/$catalog ]] \
    || die "the release payload is incomplete"
  (cd "$out" && sha256sum --check --strict "$archive.sha256" >/dev/null) \
    || die "the release payload does not match its checksum"
  [[ $(jq -er '.archive_sha256' "$out/$catalog") == "$(sha256sum "$out/$archive" | awk '{print $1}')" ]] \
    || die "the catalog names other bytes than the payload it ships"
  [[ $(jq -er '.image_sha256' "$out/$catalog") =~ ^[0-9a-f]{64}$ ]] \
    || die "the catalog names no image"

  started=$SECONDS
  bundle_work="$(mktemp -d "${TMPDIR:-/var/tmp}/tilo-internal-bundle.XXXXXX")"
  trap 'rm -rf "$bundle_work"' EXIT
  TMPDIR=/var/tmp bash os/dev/export-internal-bundle.sh \
    "$out/disk.qcow2" "$arch" "$release_tag" "$bundle_work/bundle-out" \
    || die "the internal bundle did not export from the tested medium"
  mv "$bundle_work/bundle-out/bundle" "$out/bundle"
  mv "$bundle_work/bundle-out/internal-catalog-$arch.json" "$out/internal-catalog-$arch.json"
  rm -rf "$bundle_work"
  trap - EXIT
  printf 'TILO_INSTALLER_MEDIUM_TIMING phase=bundle seconds=%s\n' "$((SECONDS - started))"
fi

(cd "$out" && find . -type f ! -name artifacts.sha256 -printf '%P\n' | LC_ALL=C sort \
  | while IFS= read -r file; do sha256sum "$file"; done) > "$out/artifacts.sha256"
(cd "$out" && sha256sum --check --strict artifacts.sha256 >/dev/null) \
  || die "the artifacts do not match their own checksums"
printf 'built the %s installer release medium from %s\n' "$arch" "$source_digest"
