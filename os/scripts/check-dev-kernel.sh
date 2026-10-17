#!/bin/bash
# Fail the dev image build when its kernel cannot run dev-VM slots.
#
#   check-dev-kernel.sh [ROOT]     (default /)
#
# tilo-vmd gives every headful slot a vkms card created through configfs and
# per-seat uinput devices (docs/architecture/dev-vms.md).
# Missing any of these surfaces only when the first slot starts, as an opaque
# modprobe or mkdir error, so check the image's own kernel here instead:
#
#   vkms      a loadable module (CONFIG_DRM_VKMS=m) with the create_default_dev
#             parameter that came with its configfs interface. Built in, the
#             modprobe.d option that keeps a default card off seat0 cannot apply.
#   uinput    built in or a module (CONFIG_INPUT_UINPUT)
#   configfs  built in or a module (CONFIG_CONFIGFS_FS)
#
# os/kernel/config-fragments/tilo.config pins all three, and the kernel build
# refuses a config that drops them; this catches a base image whose kernel came
# from somewhere else.
set -euo pipefail

root="${1:-/}"
fix="pin it in os/kernel/config-fragments/tilo.config (next to CONFIG_DRM_VKMS=m), rebuild the kernel (os/kernel/Containerfile), and rebuild the base image"
fail() {
  printf 'check-dev-kernel: %s\n  why: tilo-vmd creates each headful slot'"'"'s display (vkms via configfs) and input (uinput); without it no slot session can start.\n  fix: %s\n' "$1" "$2" >&2
  exit 1
}

mapfile -t kernels < <(find "$root/usr/lib/modules" -mindepth 1 -maxdepth 1 -type d -printf '%f\n')
[ "${#kernels[@]}" -eq 1 ] || fail "expected exactly one kernel under $root/usr/lib/modules, found ${#kernels[@]}" \
  "build the dev image from a single-kernel Tilo image"
kver="${kernels[0]}"
moddir="$root/usr/lib/modules/$kver"

builtin() { [ -f "$moddir/modules.builtin" ] && grep -Eq "/$1\.ko$" "$moddir/modules.builtin"; }
module() { find "$moddir/kernel" -name "$1.ko*" -print -quit 2>/dev/null | grep -q .; }

if builtin vkms; then
  fail "vkms is built into kernel $kver" "set CONFIG_DRM_VKMS=m in os/kernel/config-fragments/tilo.config so modprobe.d can pass create_default_dev=0"
fi
module vkms || fail "kernel $kver has no vkms module (CONFIG_DRM_VKMS)" "$fix"
modinfo -b "$root" -k "$kver" -F parm vkms | grep -q '^create_default_dev:' \
  || fail "the vkms module of kernel $kver has no create_default_dev parameter, so it predates vkms configfs (Linux 6.19)" \
          "build the base image with a Linux 6.19 or newer kernel"
modprobe -d "$root" -S "$kver" --dry-run --quiet vkms \
  || fail "modprobe cannot resolve vkms for kernel $kver (modules.dep is stale or a dependency is missing)" "$fix"
builtin uinput || module uinput || fail "kernel $kver has neither a built-in nor a modular uinput (CONFIG_INPUT_UINPUT)" "$fix"
builtin configfs || module configfs || fail "kernel $kver has no configfs (CONFIG_CONFIGFS_FS)" "$fix"

printf 'check-dev-kernel: kernel %s has vkms (module, configfs), uinput and configfs\n' "$kver"
