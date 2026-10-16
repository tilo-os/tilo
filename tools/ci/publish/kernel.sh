#!/usr/bin/env bash
# The Tilo kernel (os/kernel/Containerfile): Fedora's kernel rebuilt with Tilo's config fragment
# FROM the signed native kernel buildroot that os/kernel-buildroot-snapshot.env pins, packaged as
# an RPM repository image, booted under emulation and checked against the firmware's Secure Boot.
#
#   kernel.sh build ARCH OUT    verify the pinned buildroot, build OUT/image.oci.tar and the
#                               Secure Boot probe's inputs into OUT/secureboot (buildah image,
#                               as root in a micro-VM step)
#
# kernel-acceptance.sh, beside this script, boots and checks what this builds.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"

arch=${2:-}
out=${3:-}
case $arch in
  x86_64) platform=linux/amd64 key=KERNEL_BUILDROOT_X86_64_REF ;;
  aarch64) platform=linux/arm64 key=KERNEL_BUILDROOT_AARCH64_REF ;;
  *) die 'usage: kernel.sh build ARCH OUT' ;;
esac
[[ -n $out ]] || die 'usage: kernel.sh build ARCH OUT'
[[ $(uname -m) == "$arch" ]] || die "the $arch kernel is built natively, not on $(uname -m)"

case ${1:-} in
  build)
    . os/kernel-buildroot-snapshot.env
    buildroot=${!key}
    [[ $buildroot =~ ^registry\.tilo\.so/tilo/tilo-kernel-buildroot@sha256:[0-9a-f]{64}$ ]] \
      || die "no digest-pinned native kernel buildroot for $arch"
    use_cosign
    cosign verify --key keys/cosign.pub "$buildroot" >/dev/null
    assets=$scratch/kernel-assets
    TILO_GITHUB_ASSET_CACHE=$assets TILO_ASSET_STORE= bash os/dev/cache-github-assets.sh \
      v4l2loopback-0.15.3-a7e4a4d.tar.gz "sccache-v0.17.0-$arch-unknown-linux-musl.tar.gz"
    # No compiler cache reaches checks yet; the kernel's SOURCE_DATE_EPOCH keeps its default of
    # 0 beside the fixed layer timestamps, so the build is the same from the same inputs.
    args=(--platform "$platform" --build-context "github-assets=$assets"
      --build-arg "KERNEL_BUILDROOT_REF=$buildroot" --build-arg "KERNEL_BUILD_JOBS=$(nproc)"
      --build-arg TILO_COMPILER_CACHE=off --build-arg TILO_REQUIRE_COMPILER_CACHE=0)
    # Both targets share the build stage: one layered store compiles the kernel once.
    use_buildah kernel
    # The micro-VM step's scratch disk is ext4 under a kernel of its own, where overlay works;
    # vfs would copy the multi-gigabyte buildroot for every step and fill the disk.
    export STORAGE_DRIVER=overlay
    printf '[storage]\ndriver = "overlay"\n' > "$CONTAINERS_STORAGE_CONF"
    b build --layers --timestamp 0 --format oci --file os/kernel/Containerfile "${args[@]}" \
      -t localhost/tilo-kernel:build .
    b build --layers --timestamp 0 --file os/kernel/Containerfile --target secureboot-probe-inputs \
      "${args[@]}" --output "type=local,dest=$out/secureboot" .
    rm -f "$out/image.oci.tar"
    b push --compression-format gzip --force-compression localhost/tilo-kernel:build \
      "oci-archive:$out/image.oci.tar"
    archive_digest "$out/image.oci.tar" > "$out/digest"
    printf 'built %s\n' "$(cat "$out/digest")"
    ;;
  *) die 'usage: kernel.sh build ARCH OUT' ;;
esac
