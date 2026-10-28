#!/usr/bin/env bash
# Acceptance of the Tilo kernel image-kernel-ARCH built (kernel.sh build): boot
# the packaged kernel and verify its module signatures, then prove the firmware rejects unsigned
# EFI and runs the signed probe. Both boots are emulated (TCG), so no KVM is needed.
#
#   kernel-acceptance.sh ARCH OUT   OUT holds image.oci.tar and secureboot/ from the build
#
# t runs it as root in a micro-VM step (isolation = "vm") whose userspace is the CI builder:
# the step installs what it needs, Podman included, and with no systemd in the VM, Podman keeps
# cgroups and events itself. It is apart from kernel.sh, an input of the kernel build, so that
# changing the acceptance does not rebuild the kernel.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"

arch=${1:-}
out=${2:-}
case $arch in
  x86_64) qemu=qemu-system-x86-core firmware=edk2-ovmf ;;
  # Fedora's Secure-Boot-capable aarch64 firmware keeps its variables in QEMU's uefi-vars
  # device, which is a package of its own.
  aarch64) qemu="qemu-system-aarch64-core qemu-device-uefi-vars" firmware=edk2-aarch64 ;;
  *) die 'usage: kernel-acceptance.sh ARCH OUT' ;;
esac
[[ -n $out ]] || die 'usage: kernel-acceptance.sh ARCH OUT'
[[ $(uname -m) == "$arch" ]] || die "the $arch kernel is accepted natively, not on $(uname -m)"

dnf install -y --setopt=install_weak_deps=False gcc glibc-devel binutils cpio gzip kmod \
  python3 rpm zstd dosfstools mtools podman $qemu "$firmware" >/dev/null
podman=(podman --cgroup-manager=cgroupfs --events-backend=file)
work=$scratch/kernel-acceptance
rm -rf "$work"
mkdir -p "$work/repo"
id=$("${podman[@]}" pull --quiet "oci-archive:$out/image.oci.tar")
container=$("${podman[@]}" create "$id" /unused)
"${podman[@]}" cp "$container:/repo/." "$work/repo/"
"${podman[@]}" rm "$container" >/dev/null
shopt -s nullglob
packages=("$work"/repo/tilo-kernel-core-*.rpm)
((${#packages[@]} == 1)) || die "expected one tilo-kernel-core RPM, found ${#packages[@]}"
bash os/kernel/tests/test-rpm.sh "${packages[0]}" "$work/acceptance"
probe=$out/secureboot
gen_init_cpio=$probe/gen_init_cpio
crypto=$probe/tilo-crypto
# A kernel cross-compiled on x86_64 brings the probe's programs for no machine it boots on: the
# initramfs maker as source, and no signer. Both are built here, on the kernel's architecture,
# from that kernel tree's source and from this commit's signer.
if [[ ! -e $gen_init_cpio ]]; then
  [[ -s $probe/gen_init_cpio.c ]] || die "the build left neither gen_init_cpio nor its source"
  gen_init_cpio=$work/gen_init_cpio
  gcc -O2 -o "$gen_init_cpio" "$probe/gen_init_cpio.c"
fi
if [[ ! -e $crypto ]]; then
  CARGO_PROFILE_RELEASE_LTO=false CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 \
    cargo build --locked --release -p tilo-crypto --no-default-features --bin tilo-crypto
  crypto=${CARGO_TARGET_DIR:-$repo_root/target}/release/tilo-crypto
fi
TILO_KERNEL_PATH="$probe/vmlinuz" TILO_GEN_INIT_CPIO="$gen_init_cpio" \
  TILO_CRYPTO_BIN="$crypto" TILO_SECUREBOOT_VM_APP="$probe/secureboot_vm_app.efi" \
  TILO_SECUREBOOT_VM_ARCH="$arch" TILO_SECUREBOOT_VM_WORK_ROOT="$scratch" \
  bash os/tests/secureboot_vm_probe.sh
