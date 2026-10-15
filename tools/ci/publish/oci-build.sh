#!/usr/bin/env bash
# The build half of an image: build CONTAINERFILE in CONTEXT into an OCI archive under OUT,
# with its manifest digest beside it (OUT/image.oci.tar, OUT/digest). It holds no secrets and
# pushes nothing, so it runs anywhere, also for a working tree before it lands.
#
# usage: oci-build.sh CONTEXT CONTAINERFILE OUT [--build-arg K=V]...
#
# Runs in the buildah image as root in t's sandbox: chroot isolation and the vfs driver need no
# daemon, no user namespace of their own and no writable root.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
context=${1:?usage: oci-build.sh CONTEXT CONTAINERFILE OUT [buildah args]}
containerfile=${2:?}
out=${3:?}
shift 3
store=$scratch/buildah
export STORAGE_DRIVER=vfs BUILDAH_ISOLATION=chroot
# The image's own storage.conf names an additional image store under its read-only root.
mkdir -p "$store"
printf '[storage]\ndriver = "vfs"\n' > "$store/storage.conf"
export CONTAINERS_STORAGE_CONF=$store/storage.conf
b() { buildah --root "$store/root" --runroot "$store/run" "$@"; }
mkdir -p "$out"
# A fixed timestamp keeps the image the same when its inputs are: what a signing check
# compares is then the build, not the time it ran.
b build --layers=false --timestamp 0 --format oci -f "$containerfile" -t localhost/publish:build "$@" "$context"
rm -f "$out/image.oci.tar"
b push --compression-format gzip --force-compression localhost/publish:build "oci-archive:$out/image.oci.tar"
archive_digest "$out/image.oci.tar" > "$out/digest"
[[ $(cat "$out/digest") =~ ^sha256:[0-9a-f]{64}$ ]] || die 'no manifest digest in the archive'
printf 'built %s\n' "$(cat "$out/digest")"
