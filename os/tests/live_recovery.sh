#!/usr/bin/env bash
# Source-level safety contract for live-media file recovery and Tilo removal.
set -uo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tool="$repo/os/usr/libexec/tilo-recovery"
console="$repo/os/usr/libexec/tilo-live-recovery"
installer="$repo/os/usr/libexec/tilo-install"
artifact_helper="$repo/os/usr/libexec/tilo-install-efi-artifacts"
unit="$repo/os/usr/lib/systemd/system/tilo-live-recovery.service"
fail=0
ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

for file in "$tool" "$console" "$unit" "$artifact_helper"; do
  if [ -r "$file" ]; then ok "$(basename "$file") ships"; else bad "missing $file"; fi
done
[ "$fail" = 0 ] || exit 1

if grep -Fq -- '-o ro,nosuid,nodev,noexec,noload' "$tool" \
   && grep -Fq -- '"$CRYPTSETUP" open --readonly' "$tool"; then
  ok "recovered source files are opened read-only and non-executable"
else
  bad "recovery source does not preserve the read-only mount boundary"
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/esp/EFI/Linux/bootc" "$work/esp/loader/entries"
owned_uuid=11111111-2222-3333-4444-555555555555
printf 'sealed root=UUID=%s quiet' "$owned_uuid" > "$work/esp/EFI/Linux/bootc/bootc_composefs-owned.efi"
printf 'options root=UUID=%s quiet\n' "$owned_uuid" > "$work/esp/loader/entries/tilo.conf"
printf 'sealed root=UUID=aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee quiet' > "$work/esp/EFI/Linux/bootc/foreign.efi"
ln -s bootc/bootc_composefs-owned.efi "$work/esp/EFI/Linux/symlink.efi"
owned="$($artifact_helper "$work/esp" "$owned_uuid")"
if grep -Fxq 'EFI/Linux/bootc/bootc_composefs-owned.efi' <<<"$owned" \
   && grep -Fxq 'loader/entries/tilo.conf' <<<"$owned" \
   && ! grep -Fq 'foreign.efi' <<<"$owned" \
   && ! grep -Fq 'symlink.efi' <<<"$owned"; then
  ok "nested composefs UKIs are found recursively and ownership-bound to their sealed root UUID"
else
  bad "nested boot-artifact discovery was incomplete or accepted a foreign/symlinked file: $owned"
fi

if grep -Fq 'destination must itself be a mounted external filesystem' "$tool" \
   && grep -Fq 'destination_disk" != "$source_disk' "$tool" \
   && grep -Fq 'destination has insufficient space' "$tool" \
   && grep -Fq -- '--no-preserve=ownership' "$tool"; then
  ok "copy checks mount identity, physical separation, space, and source links"
else
  bad "copy is missing a destination or path-safety check"
fi

if TILO_RECOVERY_LIBRARY=1 source "$tool" \
   && safe_relative 'alex/Documents' \
   && safe_relative '.' \
   && ! safe_relative '../etc' \
   && ! safe_relative '/etc' \
   && safe_efi_artifact 'EFI/Linux/tilo.efi' \
   && safe_efi_artifact 'loader/entries/tilo.conf' \
   && ! safe_efi_artifact 'EFI/Linux/../../vendor.efi' \
   && ! safe_efi_artifact 'EFI/BOOT/BOOTX64.EFI'; then
  ok "runtime validators accept intended paths and reject traversal or foreign EFI paths"
else
  bad "runtime path validators did not enforce their contract"
fi

if grep -Fq 'grep -qw tilo.install /proc/cmdline' "$tool" \
   && grep -Fq 'has no Tilo install marker; refusing to guess ownership' "$tool" \
   && grep -Fq '[ "$mode" = alongside ]' "$tool" \
   && grep -Fq '[ "$confirmation" = "$expected" ]' "$tool" \
   && grep -Fq 'TILO_RECOVERY_DRY_RUN' "$tool" \
   && grep -Fq 'no EFI artifact still proves ownership by this Tilo root; root was not removed' "$tool"; then
  ok "removal is live-only, ownership-bound, alongside-only, confirmed, and dry-runnable"
else
  bad "removal safety gates are incomplete"
fi

if grep -Fq 'efi_artifact=' "$installer" \
   && grep -Fq 'TILO_INSTALL_EFI_ARTIFACTS' "$installer" \
   && grep -Fq 'root_partition_uuid=' "$installer" \
   && grep -Fq 'root_filesystem_uuid=' "$installer" \
   && grep -Fq 'efi_uuid=' "$installer"; then
  ok "new installs record root ownership and only UUID-bound EFI artifacts"
else
  bad "installer does not leave a sufficiently narrow removal receipt"
fi

if grep -Fq 'ConditionKernelCommandLine=tilo.install' "$unit" \
   && grep -Fq 'TTYPath=/dev/tty2' "$unit" \
   && grep -Fq 'tilo-live-recovery.service' "$repo/os/Containerfile.installer" \
   && grep -Fq 'Ctrl+Alt+F2 for recovery tools' "$repo/crates/session/installer/src/installer_app.rs"; then
  ok "installer media exposes the recovery console on tty2 and advertises it"
else
  bad "live-media recovery entry point is missing or undiscoverable"
fi

# shellcheck source=lib/containerfile.sh
. "$repo/os/tests/lib/containerfile.sh"
if containerfile_has_copy "$repo/os/Containerfile" os/usr/libexec/tilo-recovery /usr/libexec/tilo-recovery \
   && grep -Fq '{ link = "tilo-esp-write", to = "/usr/libexec/tilo-uki-cmdline" }' "$repo/os/components.toml"; then
  ok "installed repair mode includes the recovery backend"
else
  bad "base image does not install the recovery backend used by repair mode"
fi

if grep -Fq 'act_recover_files' "$repo/os/usr/libexec/tilo-repair" \
   && sed -n '/^act_recover_files()/,/^}/p' "$repo/os/usr/libexec/tilo-repair" | grep -Fq 'require_auth'; then
  ok "on-disk repair mode can copy personal files after owner authentication"
else
  bad "repair-mode file recovery is missing or unauthenticated"
fi

exit "$fail"
