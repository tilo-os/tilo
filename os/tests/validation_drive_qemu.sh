#!/usr/bin/env bash
# Catch QEMU validation-drive argument errors before the release gate boots.
# Run on the native ARM KVM release runner; never fall back to CPU emulation.
set -euo pipefail

[ "$(uname -m)" = aarch64 ] || { echo 'validation drive smoke requires an ARM64 host' >&2; exit 1; }
[ -c /dev/kvm ] || { echo 'validation drive smoke requires /dev/kvm' >&2; exit 1; }
command -v qemu-system-aarch64 >/dev/null || { echo 'qemu-system-aarch64 is missing' >&2; exit 1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
truncate -s 1M "$work/tilo-validation.raw"

# These are vm.sh's ARM machine/accelerator and validation-drive arguments.
# A paused VM and QMP quit prove QEMU accepted the device without booting.
qmp_out="$(printf '{"execute":"qmp_capabilities"}\n{"execute":"quit"}\n' \
  | timeout 15 qemu-system-aarch64 \
      -machine virt,gic-version=max -enable-kvm -cpu host -m 128 \
      -nodefaults -display none -serial none -monitor none -S \
      -drive "file=$work/tilo-validation.raw,format=raw,if=none,id=tilo-validation,readonly=on" \
      -device virtio-blk-pci,drive=tilo-validation,serial=tilo-validation \
      -qmp stdio 2>&1)" || { echo "$qmp_out" >&2; exit 1; }
grep -Fq '"QMP"' <<<"$qmp_out" || { echo "QEMU did not enter QMP: $qmp_out" >&2; exit 1; }
echo 'native ARM KVM accepted the read-only validation disk and virtio serial'
