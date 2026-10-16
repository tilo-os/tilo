#!/usr/bin/env bash
# The Tilo kernel (os/kernel/Containerfile): Fedora's kernel rebuilt with Tilo's config fragment
# FROM the signed native kernel buildroot that os/kernel-buildroot-snapshot.env pins, packaged as
# an RPM repository image, booted under KVM and checked against the firmware's Secure Boot.
#
#   kernel.sh build ARCH OUT    verify the pinned buildroot, build OUT/image.oci.tar and the
#                               Secure Boot probe's inputs into OUT/secureboot (buildah image)
#   kernel.sh accept ARCH OUT   boot the packaged kernel and verify its module signatures, then
#                               prove the firmware rejects unsigned EFI and runs the signed probe
#                               (a privileged node with KVM, as root)
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"

arch=${2:-}
out=${3:-}
case $arch in
  x86_64) platform=linux/amd64 jobs=6 key=KERNEL_BUILDROOT_X86_64_REF ;;
  aarch64) platform=linux/arm64 jobs=4 key=KERNEL_BUILDROOT_AARCH64_REF ;;
  *) die 'usage: kernel.sh build|accept ARCH OUT' ;;
esac
[[ -n $out ]] || die 'usage: kernel.sh build|accept ARCH OUT'
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
      --build-arg "KERNEL_BUILDROOT_REF=$buildroot" --build-arg "KERNEL_BUILD_JOBS=$jobs"
      --build-arg TILO_COMPILER_CACHE=off --build-arg TILO_REQUIRE_COMPILER_CACHE=0)
    # Both targets share the build stage: one layered store compiles the kernel once.
    use_buildah kernel
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
  accept)
    case $arch in
      x86_64) qemu=qemu-system-x86-core firmware=edk2-ovmf ;;
      aarch64) qemu=qemu-system-aarch64-core firmware=edk2-aarch64 ;;
    esac
    dnf install -y --setopt=install_weak_deps=False gcc glibc-devel binutils cpio gzip kmod \
      python3 rpm zstd dosfstools mtools "$qemu" "$firmware" >/dev/null
    work=$scratch/kernel-acceptance
    rm -rf "$work"
    mkdir -p "$work/repo"
    id=$(podman pull --quiet "oci-archive:$out/image.oci.tar")
    container=$(podman create "$id" /unused)
    podman cp "$container:/repo/." "$work/repo/"
    podman rm "$container" >/dev/null
    shopt -s nullglob
    packages=("$work"/repo/tilo-kernel-core-*.rpm)
    ((${#packages[@]} == 1)) || die "expected one tilo-kernel-core RPM, found ${#packages[@]}"
    bash os/kernel/tests/test-rpm.sh "${packages[0]}" "$work/acceptance"
    probe=$out/secureboot
    TILO_KERNEL_PATH="$probe/vmlinuz" TILO_GEN_INIT_CPIO="$probe/gen_init_cpio" \
      TILO_CRYPTO_BIN="$probe/tilo-crypto" TILO_SECUREBOOT_VM_APP="$probe/secureboot_vm_app.efi" \
      TILO_SECUREBOOT_VM_ARCH="$arch" TILO_SECUREBOOT_VM_WORK_ROOT="$scratch" \
      bash os/tests/secureboot_vm_probe.sh
    ;;
  *) die 'usage: kernel.sh build|accept ARCH OUT' ;;
esac
