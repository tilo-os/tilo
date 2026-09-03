#!/usr/bin/env bash
# Source-level safety contract for live-media file recovery and qOS removal.
set -uo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tool="$repo/os/usr/libexec/qos-recovery"
console="$repo/os/usr/libexec/qos-live-recovery"
installer="$repo/os/usr/libexec/qos-install"
unit="$repo/os/usr/lib/systemd/system/qos-live-recovery.service"
fail=0
ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

for file in "$tool" "$console" "$unit"; do
  if [ -r "$file" ]; then ok "$(basename "$file") ships"; else bad "missing $file"; fi
done
[ "$fail" = 0 ] || exit 1

if grep -Fq -- '-o ro,nosuid,nodev,noexec,noload' "$tool" \
   && grep -Fq -- '"$CRYPTSETUP" open --readonly' "$tool"; then
  ok "recovered source files are opened read-only and non-executable"
else
  bad "recovery source does not preserve the read-only mount boundary"
fi

if grep -Fq 'destination must itself be a mounted external filesystem' "$tool" \
   && grep -Fq 'destination_disk" != "$source_disk' "$tool" \
   && grep -Fq 'destination has insufficient space' "$tool" \
   && grep -Fq -- '--no-preserve=ownership' "$tool"; then
  ok "copy checks mount identity, physical separation, space, and source links"
else
  bad "copy is missing a destination or path-safety check"
fi

if QOS_RECOVERY_LIBRARY=1 source "$tool" \
   && safe_relative 'alex/Documents' \
   && safe_relative '.' \
   && ! safe_relative '../etc' \
   && ! safe_relative '/etc' \
   && safe_efi_artifact 'EFI/Linux/qos.efi' \
   && safe_efi_artifact 'loader/entries/qos.conf' \
   && ! safe_efi_artifact 'EFI/Linux/../../vendor.efi' \
   && ! safe_efi_artifact 'EFI/BOOT/BOOTX64.EFI'; then
  ok "runtime validators accept intended paths and reject traversal or foreign EFI paths"
else
  bad "runtime path validators did not enforce their contract"
fi

if grep -Fq 'grep -qw qos.install /proc/cmdline' "$tool" \
   && grep -Fq 'has no qOS install marker; refusing to guess ownership' "$tool" \
   && grep -Fq '[ "$mode" = alongside ]' "$tool" \
   && grep -Fq '[ "$confirmation" = "$expected" ]' "$tool" \
   && grep -Fq 'QOS_RECOVERY_DRY_RUN' "$tool"; then
  ok "removal is live-only, ownership-bound, alongside-only, confirmed, and dry-runnable"
else
  bad "removal safety gates are incomplete"
fi

if grep -Fq 'efi_artifact=' "$installer" \
   && grep -Fq 'root=UUID=${ROOT_FS_UUID' "$installer" \
   && grep -Fq 'root_partition_uuid=' "$installer" \
   && grep -Fq 'root_filesystem_uuid=' "$installer" \
   && grep -Fq 'efi_uuid=' "$installer"; then
  ok "new installs record root ownership and only UUID-bound EFI artifacts"
else
  bad "installer does not leave a sufficiently narrow removal receipt"
fi

if grep -Fq 'ConditionKernelCommandLine=qos.install' "$unit" \
   && grep -Fq 'TTYPath=/dev/tty2' "$unit" \
   && grep -Fq 'qos-live-recovery.service' "$repo/os/Containerfile.installer" \
   && grep -Fq 'Ctrl+Alt+F2 for recovery tools' "$repo/crates/session/installer/src/installer_app.rs"; then
  ok "installer media exposes the recovery console on tty2 and advertises it"
else
  bad "live-media recovery entry point is missing or undiscoverable"
fi

if grep -Fq 'COPY os/usr/libexec/qos-recovery /usr/libexec/qos-recovery' "$repo/os/Containerfile" \
   && grep -Fq '/usr/libexec/qos-recovery /usr/libexec/qos-uki-cmdline' "$repo/os/Containerfile"; then
  ok "installed repair mode includes the recovery backend"
else
  bad "base image does not install the recovery backend used by repair mode"
fi

if grep -Fq 'act_recover_files' "$repo/os/usr/libexec/qos-repair" \
   && sed -n '/^act_recover_files()/,/^}/p' "$repo/os/usr/libexec/qos-repair" | grep -Fq 'require_auth'; then
  ok "on-disk repair mode can copy personal files after owner authentication"
else
  bad "repair-mode file recovery is missing or unauthenticated"
fi

exit "$fail"
