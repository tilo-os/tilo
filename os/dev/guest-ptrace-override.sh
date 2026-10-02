#!/usr/bin/env bash
# Only for a disposable, unsealed dev VM overlay used to locate a guest fault.
set -euo pipefail

source=/usr/lib/sysctl.d/60-tilo-hardening.conf
override=/etc/sysctl.d/60-tilo-hardening.conf
grep -qx 'kernel.yama.ptrace_scope = 3' "$source"
mkdir -p /etc/sysctl.d
sed 's/^kernel.yama.ptrace_scope = 3$/kernel.yama.ptrace_scope = 0/' \
  "$source" > "$override"
grep -qx 'kernel.yama.ptrace_scope = 0' "$override"
cmdline_initrd=$(tr ' ' '\n' </proc/cmdline | sed -n 's/^initrd=//p')
[[ -n "$cmdline_initrd" ]]
boot_initrd="/boot/${cmdline_initrd//\\//}"
[[ -s "$boot_initrd" ]]
kver=$(uname -r)
rebuilt_initrd=/tmp/tilo-ptrace-initrd.img
dracut --force --reproducible --compress zstd --kver "$kver" \
  --omit 'network network-legacy network-manager nfs iscsi nvmf nbd fcoe fcoe-uefi nbde clevis clevis-pin-null clevis-pin-sss clevis-pin-tang clevis-pin-tpm2 crypt-ssh' \
  --omit-drivers xfs --add-drivers ext4 \
  --add 'crypt systemd-cryptsetup tpm2-tss plymouth tilo-rollback' \
  --include "$override" "$override" "$rebuilt_initrd"
[[ -s "$rebuilt_initrd" ]]
mount -o remount,rw /boot
cp "$rebuilt_initrd" "$boot_initrd.new"
mv "$boot_initrd.new" "$boot_initrd"
sync
