#!/usr/bin/env bash
# The OS assembler image (os/image/assembler.Containerfile) for ARCH, into OUT/oci, as
# root in a micro-VM step. The one place a build installs podman into its guest: every
# other OS action runs in the image this makes.
#
# usage: assembler.sh ARCH OUT
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/../../tools/ci/publish/lib.sh"
cd "$repo_root"
arch=${1:?usage: assembler.sh ARCH OUT}
out=${2:?}
[[ $(uname -m) == "$arch" ]] || die "the $arch assembler builds natively, not on $(uname -m)"
command -v podman >/dev/null || dnf install -y --setopt=install_weak_deps=False podman skopeo jq >/dev/null
. os/base-snapshot.env
status=0
bash os/dev/resolve-rpm-snapshot.sh --exists || status=$?
case $status in
  0) RPMS_REF=$(bash os/dev/resolve-rpm-snapshot.sh) ;;
  1)
    built=/target/publish/rpms/$arch
    [[ -f $built/image.oci.tar ]] || die "no published RPM snapshot for this package set and no image-rpms-$arch in this run"
    RPMS_REF=localhost/tilo-rpms:assembler
    podman tag "$(podman pull -q "oci-archive:$built/image.oci.tar")" "$RPMS_REF" ;;
  *) die 'could not ask the registry for the published RPM snapshot' ;;
esac
epoch=$(bash os/dev/image-epoch.sh "$BASE_REF" "$RPMS_REF")
podman build --security-opt label=disable --format oci --source-date-epoch "$epoch" --rewrite-timestamp \
  --build-arg "BASE_REF=$BASE_REF" --build-arg "RPMS_REF=$RPMS_REF" \
  -f os/image/assembler.Containerfile -t localhost/tilo-assembler:latest os/image
rm -rf "$out"
mkdir -p "$out"
podman push -q --compression-format gzip --force-compression localhost/tilo-assembler:latest "oci:$out/oci:tilo"
printf 'assembler %s for %s\n' "$(podman image inspect --format '{{.Digest}}' localhost/tilo-assembler:latest)" "$arch"
