#!/usr/bin/env bash
# The build half of the portable FEX payload (os/Containerfile.fex-payload): the pinned FEX
# source compiled offline on aarch64 in the toolchain image against the x86 SDK, both taken
# from the archives their checks built and named by the digests they are published with, so
# what the payload records (inputs.env) points at the published images.
#
# usage: fex-payload.sh OUT BUILDROOT_OUT SDK_OUT SOURCE_ARCHIVE
#
# Runs in the buildah image as root in a micro-VM step. Unlike oci-build.sh it stores images
# with the overlay driver: both inputs sit on the Fedora bootc base's many layers, which vfs
# copies whole once per layer, more than the step's scratch disk holds.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
out=${1:?usage: fex-payload.sh OUT BUILDROOT_OUT SDK_OUT SOURCE_ARCHIVE}
buildroot_out=${2:?}
sdk_out=${3:?}
source_archive=${4:?}
[[ $(uname -m) == aarch64 ]] || die 'the FEX payload is built on aarch64'
[[ $(id -u) == 0 ]] || die 'runs as root in a micro-VM step (isolation = "vm")'
store=$scratch/buildah
mkdir -p "$store"
printf '[storage]\ndriver = "overlay"\n' > "$store/storage.conf"
export CONTAINERS_STORAGE_CONF=$store/storage.conf STORAGE_DRIVER=overlay BUILDAH_ISOLATION=chroot
buildah_root=$store/root
buildah_runroot=$store/run

# The image of DIR/image.oci.tar in the store under REPOSITORY, by the reference it is
# published as: REPOSITORY@<its digest>. The build takes it FROM there without pulling.
preload() {
  local repository=$1 dir=$2 digest id
  digest=$(cat "$dir/digest")
  [[ $(archive_digest "$dir/image.oci.tar") == "$digest" ]] || die "$dir/image.oci.tar is not its build"
  id=$(b pull --quiet "oci-archive:$dir/image.oci.tar")
  b tag "$id" "$repository:local" >&2
  b inspect --type image "$repository@$digest" >/dev/null || die "$repository@$digest does not resolve to $dir"
  printf '%s@%s' "$repository" "$digest"
}

buildroot=$(preload registry.tilo.so/tilo/tilo-fex-buildroot-candidate "$buildroot_out")
sdk=$(preload registry.tilo.so/tilo/tilo-fex-x86-sdk-candidate "$sdk_out")
src=$scratch/fex-source
rm -rf "$src"
mkdir -p "$src"
# Owned by root, who builds it, so Git trusts the checkout build-fex-payload.sh verifies.
tar -C "$src" --no-same-owner -xf "$source_archive"
# Bounded by the step's cores and memory: about 4 GiB per compiler.
jobs=$(awk '/^MemTotal:/ {print int($2 / 4194304)}' /proc/meminfo)
(( jobs > $(nproc) )) && jobs=$(nproc)
(( jobs < 1 )) && jobs=1
printf 'FEX payload build jobs=%s (cores %s)\n' "$jobs" "$(nproc)"
mkdir -p "$out"
# A fixed timestamp keeps the image the same when its inputs are, as in oci-build.sh.
b build --layers=false --timestamp 0 --format oci --pull=never -f "$repo_root/os/Containerfile.fex-payload" \
  -t localhost/publish:build --build-arg "FEX_BUILDROOT_REF=$buildroot" \
  --build-arg "FEX_X86_SDK_REF=$sdk" --build-arg "BUILD_JOBS=$jobs" \
  --build-context "fex-source=$src" --label org.tilo.development-only=true "$repo_root"
rm -f "$out/image.oci.tar"
b push --compression-format gzip --force-compression localhost/publish:build "oci-archive:$out/image.oci.tar"
archive_digest "$out/image.oci.tar" > "$out/digest"
[[ $(cat "$out/digest") =~ ^sha256:[0-9a-f]{64}$ ]] || die 'no manifest digest in the archive'
printf 'built %s\n' "$(cat "$out/digest")"
