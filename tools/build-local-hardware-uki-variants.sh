#!/bin/bash
# Build unsigned live/target UKI variants from already verified local inputs.
#
# This is the fast bring-up path for hardware work: it does not rebuild or
# unpack an OCI image. The caller supplies the image's existing UKI identity,
# kernel, hardware-ready initramfs and DTB. The resulting target differs from
# the live variant only by the `tilo.install` command-line marker.
set -euo pipefail

usage() {
  echo "usage: $0 BASE_UKI KERNEL INITRAMFS DTB KERNEL_VERSION OUTPUT_DIR" >&2
  exit 2
}
[ "$#" -eq 7 ] || usage
base_uki="$1"
kernel="$2"
initramfs="$3"
dtb="$4"
kernel_version="$5"
output_dir="$6"
stub="$7"

for input in "$base_uki" "$kernel" "$initramfs" "$dtb" "$stub"; do
  [ -s "$input" ] || { echo "missing or empty input: $input" >&2; exit 1; }
done
[[ "$kernel_version" =~ ^[A-Za-z0-9][A-Za-z0-9._+-]*$ ]] \
  || { echo "invalid kernel version: $kernel_version" >&2; exit 1; }
mkdir -p "$output_dir"
work="$(mktemp -d)"
trap 'rm -rf -- "$work"' EXIT

# objcopy rewrites its input when no output operand is supplied. Always write
# a disposable output while dumping both sections, keeping BASE_UKI immutable.
objcopy \
  --dump-section .osrel="$work/os-release" \
  --dump-section .cmdline="$work/cmdline.raw" \
  "$base_uki" "$work/inspect-copy.efi"
tr -d '\000' < "$work/cmdline.raw" | sed 's/[[:space:]]*$//' > "$work/target.cmdline"
grep -qw root=tilo-auto "$work/target.cmdline" \
  || { echo 'base UKI lacks root=tilo-auto' >&2; exit 1; }
if grep -qw tilo.install "$work/target.cmdline"; then
  echo 'base UKI unexpectedly contains tilo.install' >&2
  exit 1
fi
mount_contract='systemd.mount-extra=LABEL=TILO_BOOT:/boot:ext4:ro,nosuid,nodev,noexec'
if ! grep -qw "$mount_contract" "$work/target.cmdline"; then
  printf ' %s' "$mount_contract" >> "$work/target.cmdline"
fi
printf '\n' >> "$work/target.cmdline"
printf '%s tilo.install\n' "$(cat "$work/target.cmdline")" > "$work/live.cmdline"

common=(
  --linux "$kernel"
  --initrd "$initramfs"
  --os-release "@$work/os-release"
  --uname "$kernel_version"
  --devicetree "$dtb"
  --stub "$stub"
  --efi-arch aa64
  --no-measure
)
live="$output_dir/tilo-hardware-live.efi"
target="$output_dir/tilo-hardware-target.efi"
ukify build "${common[@]}" --cmdline "@$work/live.cmdline" --output "$live"
ukify build "${common[@]}" --cmdline "@$work/target.cmdline" --output "$target"

ukify inspect "$live" > "$output_dir/live-inspect.txt"
ukify inspect "$target" > "$output_dir/target-inspect.txt"
for image in "$live" "$target"; do
  objdump -h "$image" | grep -q '[.]dtb' || { echo "$image lacks .dtb" >&2; exit 1; }
  objdump -h "$image" | grep -q '[.]initrd' || { echo "$image lacks .initrd" >&2; exit 1; }
done
grep -qw tilo.install "$output_dir/live-inspect.txt"
if grep -qw tilo.install "$output_dir/target-inspect.txt"; then
  echo 'target UKI still contains tilo.install' >&2
  exit 1
fi
grep -qw "$mount_contract" "$output_dir/live-inspect.txt"
grep -qw "$mount_contract" "$output_dir/target-inspect.txt"
sha256sum "$live" "$target" > "$output_dir/SHA256SUMS"
