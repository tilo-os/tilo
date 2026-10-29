#!/usr/bin/env bash
# The build half of an image: build CONTAINERFILE in CONTEXT into an OCI archive under OUT,
# with its manifest digest beside it (OUT/image.oci.tar, OUT/digest). It holds no secrets and
# pushes nothing, so it runs anywhere, also for a working tree before it lands.
#
# usage: oci-build.sh CONTEXT CONTAINERFILE OUT [--build-arg K=V]...
#
# Runs in the buildah image as root in a micro-VM step (isolation = "vm"): chroot isolation needs
# no daemon, and the overlay driver works on the step's own ext4 scratch disk. vfs, which needs
# nothing, copies every layer whole, so an image FROM the multi-GB CI builder or the bootc base
# filled the step's memory and disk.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/build-lib.sh"
context=${1:?usage: oci-build.sh CONTEXT CONTAINERFILE OUT [buildah args]}
containerfile=${2:?}
out=${3:?}
shift 3
store=$scratch/buildah
driver=${TILO_BUILDAH_DRIVER:-overlay}
export STORAGE_DRIVER=$driver BUILDAH_ISOLATION=chroot
# The image's own storage.conf names an additional image store under its read-only root.
mkdir -p "$store"
printf '[storage]\ndriver = "%s"\n' "$driver" > "$store/storage.conf"
export CONTAINERS_STORAGE_CONF=$store/storage.conf
b() { buildah --root "$store/root" --runroot "$store/run" "$@"; }
mkdir -p "$out"
# The commit's time keeps the image the same when its inputs are: what a signing check
# compares is then the build, not the time it ran. It dates every file in the image too, and
# not 0: a web server names no ETag or Last-Modified for a file dated 0 or 1 (Caddy does not),
# and one dated alike across releases would be told apart by its size alone.
timestamp=${SOURCE_DATE_EPOCH:-$(git -C "$(dirname -- "${BASH_SOURCE[0]}")" log -1 --format=%ct HEAD)}
[[ $timestamp =~ ^[0-9]+$ && $timestamp -gt 1 ]] || die "no commit time to date the image by: $timestamp"
# buildah refuses --timestamp beside a SOURCE_DATE_EPOCH in its environment.
unset SOURCE_DATE_EPOCH
b build --layers=false --timestamp "$timestamp" --format oci -f "$containerfile" -t localhost/publish:build "$@" "$context"
rm -f "$out/image.oci.tar"
b push --compression-format gzip --force-compression localhost/publish:build "oci-archive:$out/image.oci.tar"
archive_digest "$out/image.oci.tar" > "$out/digest"
[[ $(cat "$out/digest") =~ ^sha256:[0-9a-f]{64}$ ]] || die 'no manifest digest in the archive'
printf 'built %s\n' "$(cat "$out/digest")"
