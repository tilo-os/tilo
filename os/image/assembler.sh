#!/usr/bin/env bash
# The OS assembler image (os/image/assembler.Containerfile) for ARCH, into OUT/oci, as
# root in a micro-VM step. The one place a build installs podman into its guest: every
# other OS action runs in the image this makes.
#
# usage: assembler.sh ARCH OUT [SNAPSHOT]
#
# SNAPSHOT is rpm-snapshot-ARCH's directory, which a run of t hands this step: the snapshot it
# has already resolved and verified (lib.sh snapshot_for_build) — the published set of this
# architecture, or, where no release tag has published it, this run's image-rpms-ARCH, loaded
# unsigned. A build outside t (tools/ci/build-os.sh) passes none and resolves it here.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/../publish/lib.sh"
cd "$repo_root"
arch=${1:?usage: assembler.sh ARCH OUT [SNAPSHOT]}
out=${2:?}
snapshot=${3:-}
[[ $(uname -m) == "$arch" ]] || die "the $arch assembler builds natively, not on $(uname -m)"
command -v podman >/dev/null || dnf install -y --setopt=install_weak_deps=False podman skopeo jq >/dev/null
. os/base-snapshot.env
rpms=/target/publish/rpms/$arch
if [[ -n $snapshot ]]; then
  # The image installs with podman, so that is the store the run's own archive is loaded into.
  snapshot_for_build "$snapshot" "$rpms" "$arch" podman
else
  # Resolved here, as the OS layers resolve it: published where a release tag has published it,
  # else this run's own set, unsigned.
  status=0
  bash os/dev/resolve-rpm-snapshot.sh --exists --arch "$arch" || status=$?
  case $status in
    0)
      use_cosign
      rpms_ref=$(bash os/dev/resolve-rpm-snapshot.sh --arch "$arch")
      [[ $rpms_ref =~ ^registry\.tilo\.so/tilo/rpms@sha256:[0-9a-f]{64}$ ]] || die "unexpected snapshot $rpms_ref"
      rpms_source=$rpms_ref ;;
    1)
      [[ -f $rpms/image.oci.tar ]] || die "no published RPM snapshot for this package set and no image-rpms-$arch in this run"
      [[ $(archive_digest "$rpms/image.oci.tar") == "$(cat "$rpms/digest")" ]] || die "$rpms/image.oci.tar is not the build"
      rpms_ref=localhost/tilo-rpms:$(bash os/dev/resolve-rpm-snapshot.sh --tag --arch "$arch")-$arch
      podman tag "$(podman pull -q "oci-archive:$rpms/image.oci.tar")" "$rpms_ref"
      rpms_source="unsigned image-rpms-$arch $(cat "$rpms/digest")" ;;
    *) die 'could not ask the registry for the published RPM snapshot' ;;
  esac
fi
RPMS_REF=$rpms_ref
# An unpublished snapshot is dated by its newest package signature (rpms-build.sh), not by when
# it was resolved.
if [[ $rpms_source != "$rpms_ref" ]]; then
  rpms_epoch=$(cat "$rpms/epoch")
  [[ $rpms_epoch =~ ^[1-9][0-9]*$ ]] || die "image-rpms-$arch left no clock in $rpms/epoch"
  epoch=$(bash os/dev/image-epoch.sh "$BASE_REF")
  if (( rpms_epoch > epoch )); then epoch=$rpms_epoch; fi
  printf 'no published RPM snapshot for this package set: installing from image-rpms-%s, unsigned\n' "$arch"
else
  epoch=$(bash os/dev/image-epoch.sh "$BASE_REF" "$RPMS_REF")
fi
podman build --security-opt label=disable --format oci --source-date-epoch "$epoch" --rewrite-timestamp \
  --build-arg "BASE_REF=$BASE_REF" --build-arg "RPMS_REF=$RPMS_REF" \
  -f os/image/assembler.Containerfile -t localhost/tilo-assembler:latest os/image
rm -rf "$out"
mkdir -p "$out"
podman push -q --compression-format gzip --force-compression localhost/tilo-assembler:latest "oci:$out/oci:tilo"
printf 'assembler %s for %s\n' "$(podman image inspect --format '{{.Digest}}' localhost/tilo-assembler:latest)" "$arch"
