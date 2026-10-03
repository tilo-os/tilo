#!/usr/bin/env bash
# UEFI-like filesystem fixtures for alongside boot-menu integration.
set -uo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
menu="$repo/os/usr/libexec/tilo-install-boot-menu"
artifacts="$repo/os/usr/libexec/tilo-install-efi-artifacts"
installer="$repo/os/usr/libexec/tilo-install"
fail=0
ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
uuid=11111111-2222-3333-4444-555555555555

esp="$work/esp"
mkdir -p "$esp/EFI/Microsoft/Boot" "$esp/EFI/ubuntu" "$esp/EFI/fedora"
printf 'windows binary' > "$esp/EFI/Microsoft/Boot/bootmgfw.efi"
printf 'ubuntu shim' > "$esp/EFI/ubuntu/shimx64.efi"
printf 'ubuntu grub' > "$esp/EFI/ubuntu/grubx64.efi"
printf 'fedora grub' > "$esp/EFI/fedora/grubx64.efi"
output="$(TILO_BOOT_MENU_ARCH=x86_64 "$menu" install "$esp" "$uuid")" || bad "boot menu fixture installs"
if grep -Fxq loader/entries/tilo-windows.conf <<<"$output" \
   && grep -Fxq loader/entries/tilo-linux-ubuntu.conf <<<"$output" \
   && grep -Fxq loader/entries/tilo-linux-fedora.conf <<<"$output" \
   && grep -Fqx 'efi /EFI/Microsoft/Boot/bootmgfw.efi' "$esp/loader/entries/tilo-windows.conf" \
   && grep -Fqx 'efi /EFI/ubuntu/shimx64.efi' "$esp/loader/entries/tilo-linux-ubuntu.conf" \
   && grep -Fqx 'efi /EFI/fedora/grubx64.efi' "$esp/loader/entries/tilo-linux-fedora.conf"; then
  ok "Windows and Linux loaders get deterministic same-ESP systemd-boot entries"
else
  bad "Windows/Linux entry contents are incomplete"
fi

arm="$work/arm"
mkdir -p "$arm/EFI/debian"
printf armshim > "$arm/EFI/debian/shimaa64.efi"
arm_output="$(TILO_BOOT_MENU_ARCH=aarch64 "$menu" install "$arm" "$uuid")"
if grep -Fxq loader/entries/tilo-linux-debian.conf <<<"$arm_output" \
   && grep -Fqx 'efi /EFI/debian/shimaa64.efi' "$arm/loader/entries/tilo-linux-debian.conf"; then
  ok "ARM64 Linux shim discovery uses the shipped systemd-boot efi-entry syntax"
else
  bad "ARM64 loader discovery or entry syntax is wrong"
fi

owned="$($artifacts "$esp" "$uuid")"
if grep -Fxq loader/entries/tilo-windows.conf <<<"$owned" \
   && grep -Fxq loader/entries/tilo-linux-ubuntu.conf <<<"$owned" \
   && grep -Fxq loader/entries/tilo-linux-fedora.conf <<<"$owned"; then
  ok "chainloader entries are bound to the Tilo root ownership receipt"
else
  bad "chainloader entries are not discoverable for owned cleanup"
fi

# Removal consumes exactly the ownership helper's paths. Exercise that contract
# on a FAT-like tree and prove the foreign EFI programs remain byte-for-byte.
before="$(sha256sum "$esp/EFI/Microsoft/Boot/bootmgfw.efi" "$esp/EFI/ubuntu/shimx64.efi" "$esp/EFI/fedora/grubx64.efi")"
while IFS= read -r relative; do [ -z "$relative" ] || rm -f -- "$esp/$relative"; done <<<"$owned"
after="$(sha256sum "$esp/EFI/Microsoft/Boot/bootmgfw.efi" "$esp/EFI/ubuntu/shimx64.efi" "$esp/EFI/fedora/grubx64.efi")"
if [ "$before" = "$after" ] && [ -f "$esp/EFI/Microsoft/Boot/bootmgfw.efi" ] \
   && [ ! -e "$esp/loader/entries/tilo-windows.conf" ]; then
  ok "owned-entry removal leaves every foreign EFI binary untouched"
else
  bad "cleanup changed a foreign EFI loader or retained an owned entry"
fi

ambiguous="$work/ambiguous"
mkdir -p "$ambiguous/EFI/Microsoft/Boot" "$ambiguous/EFI/microsoft/boot"
printf one > "$ambiguous/EFI/Microsoft/Boot/bootmgfw.efi"
printf two > "$ambiguous/EFI/microsoft/boot/bootmgfw.efi"
if TILO_BOOT_MENU_ARCH=x86_64 "$menu" install "$ambiguous" "$uuid" >"$work/ambiguous.out" 2>&1; then
  bad "ambiguous Windows loaders were accepted"
elif [ ! -e "$ambiguous/loader/entries/tilo-windows.conf" ]; then
  ok "ambiguous loader discovery fails before writing an entry"
else
  bad "ambiguous discovery left a partial boot-menu entry"
fi

symlinked="$work/symlinked"
mkdir -p "$symlinked/EFI/ubuntu" "$work/outside"
printf shim > "$symlinked/EFI/ubuntu/shimx64.efi"
ln -s "$work/outside" "$symlinked/loader"
if TILO_BOOT_MENU_ARCH=x86_64 "$menu" install "$symlinked" "$uuid" >"$work/symlink.out" 2>&1; then
  bad "symlinked loader entry directory was accepted"
elif [ ! -e "$work/outside/entries/tilo-linux-ubuntu.conf" ]; then
  ok "symlinked output directories fail closed without traversal"
else
  bad "boot entry escaped through a symlink"
fi

owned_conf="$work/foreign-owned/EFI/ubuntu"
mkdir -p "$owned_conf" "$work/foreign-owned/loader/entries"
printf shim > "$owned_conf/shimx64.efi"
printf 'title foreign\n' > "$work/foreign-owned/loader/entries/tilo-linux-ubuntu.conf"
if TILO_BOOT_MENU_ARCH=x86_64 "$menu" install "$work/foreign-owned" "$uuid" >"$work/foreign.out" 2>&1; then
  bad "a pre-existing foreign Tilo-prefixed filename was overwritten"
else
  ok "pre-existing entries require the exact Tilo root owner marker"
fi

# efibootmgr is injected here so the fixture can prove that bootc-created
# ordering/default changes are rolled back exactly, without touching host NVRAM.
mkdir -p "$work/efivars"
guid=8be4df61-93ca-11d2-aa0d-00e098032b8c
# Binary efivarfs attributes + load-option attributes/path length, UTF-16
# description, end device-path node, and opaque optional bytes (including NUL).
for id in 0000 0002 0003 000A; do
  printf '\007\000\000\000\001\000\000\000\004\000W\000\000\000\177\377\004\000\000\012\377' \
    > "$work/efivars/Boot$id-$guid"
done
cp "$work/efivars/Boot000A-$guid" "$work/original-entry"
fake="$work/efibootmgr"
cat > "$fake" <<'EOF'
#!/usr/bin/env bash
if [ "$#" -eq 0 ]; then
  printf 'BootCurrent: 0002\n'
  if [ "${TILO_FAKE_BOOT_NEXT:-0003}" != absent ]; then
    printf 'BootNext: %s\n' "${TILO_FAKE_BOOT_NEXT:-0003}"
  fi
  printf 'BootOrder: %s\n' "${TILO_FAKE_BOOT_ORDER:-0002,0000,0003}"
else
  printf '%s\n' "$*" >> "$TILO_FAKE_EFIBOOT_LOG"
fi
EOF
chmod +x "$fake"
state="$work/firmware.state"
export TILO_FAKE_EFIBOOT_LOG="$work/efiboot.log"
TILO_BOOT_MENU_EFIVARS="$work/efivars" TILO_BOOT_MENU_EFIBOOTMGR="$fake" \
  "$menu" firmware-snapshot "$state"

# A successful efibootmgr listing is insufficient when efivarfs data cannot
# supply every referenced entry. A failed snapshot must not replace state.
mkdir -p "$work/no-entries" "$work/missing-ordered"
cp "$work/original-entry" "$work/missing-ordered/Boot0000-$guid"
cp "$work/original-entry" "$work/missing-ordered/Boot0002-$guid"
for directory in no-entries missing-ordered; do
  printf 'previous snapshot\n' > "$work/incomplete.state"
  if TILO_BOOT_MENU_EFIVARS="$work/$directory" TILO_BOOT_MENU_EFIBOOTMGR="$fake" \
      "$menu" firmware-snapshot "$work/incomplete.state" > "$work/snapshot-failure" 2>&1; then
    bad "$directory snapshot accepted missing referenced firmware bytes"
  elif grep -Fqx 'previous snapshot' "$work/incomplete.state" \
      && grep -Fq 'referenced by BootOrder or BootNext' "$work/snapshot-failure"; then
    ok "$directory snapshot rejects incomplete evidence before replacing state"
  else
    bad "$directory failure changed state or lacked a useful diagnostic"
  fi
done
if TILO_FAKE_BOOT_NEXT=000c TILO_BOOT_MENU_EFIVARS="$work/efivars" TILO_BOOT_MENU_EFIBOOTMGR="$fake" \
    "$menu" firmware-snapshot "$work/next-missing.state" > "$work/snapshot-failure" 2>&1; then
  bad "BootNext-only missing entry was accepted"
elif grep -Fq 'missing readable Boot000C bytes' "$work/snapshot-failure"; then
  ok "BootNext requires its own captured entry even outside BootOrder"
else
  bad "missing BootNext entry did not report the normalized ID"
fi
if TILO_FAKE_BOOT_NEXT=000a TILO_FAKE_BOOT_ORDER=000a,0002 \
    TILO_BOOT_MENU_EFIVARS="$work/efivars" TILO_BOOT_MENU_EFIBOOTMGR="$fake" \
    "$menu" firmware-snapshot "$work/lowercase.state"; then
  ok "case-normalized firmware IDs match the captured variable name"
else
  bad "lowercase efibootmgr IDs failed to match uppercase variable names"
fi
# The new Tilo entry is not an alteration of any pre-existing foreign entry.
cp "$work/original-entry" "$work/efivars/Boot000B-$guid"
TILO_BOOT_MENU_EFIVARS="$work/efivars" TILO_BOOT_MENU_EFIBOOTMGR="$fake" \
  "$menu" firmware-restore "$state"
if grep -Fxq -- '-o 0002,0000,0003' "$TILO_FAKE_EFIBOOT_LOG" \
   && grep -Fxq -- '-n 0003' "$TILO_FAKE_EFIBOOT_LOG"; then
  ok "unchanged foreign entries permit exact BootOrder/BootNext restoration and a new Tilo entry"
else
  bad "firmware state was not faithfully restored"
fi

# Repeating restoration when no BootNext exists must not call efibootmgr -N:
# real efibootmgr returns ENOENT for that redundant operation.
: > "$TILO_FAKE_EFIBOOT_LOG"
TILO_FAKE_BOOT_NEXT=absent TILO_BOOT_MENU_EFIVARS="$work/efivars" TILO_BOOT_MENU_EFIBOOTMGR="$fake" \
  "$menu" firmware-snapshot "$work/no-next.state"
TILO_FAKE_BOOT_NEXT=absent TILO_BOOT_MENU_EFIVARS="$work/efivars" TILO_BOOT_MENU_EFIBOOTMGR="$fake" \
  "$menu" firmware-restore "$work/no-next.state"
if grep -Fxq -- '-o 0002,0000,0003' "$TILO_FAKE_EFIBOOT_LOG" \
   && ! grep -Eq -- '^-N$|^-n ' "$TILO_FAKE_EFIBOOT_LOG"; then
  ok "already absent BootNext needs no deletion"
else
  bad "restoring an absent BootNext attempted an unnecessary write"
fi

if grep -Fqx "boot_entry=Boot000A-$guid=07000000010000000400570000007fff0400000aff" "$state"; then
  ok "snapshot retains exact binary bytes for an entry outside BootOrder"
else
  bad "snapshot lost efivar attributes, device-path or optional-data bytes"
fi
cp "$state" "$work/original-state"

reject_changed_entry() {
  local description="$1"
  : > "$TILO_FAKE_EFIBOOT_LOG"
  if TILO_BOOT_MENU_EFIVARS="$work/efivars" TILO_BOOT_MENU_EFIBOOTMGR="$fake" \
      "$menu" firmware-restore "$state" > "$work/restore-failure" 2>&1; then
    bad "$description was accepted"
  elif [ ! -s "$TILO_FAKE_EFIBOOT_LOG" ] \
      && cmp -s "$state" "$work/original-state" \
      && grep -Fq 'entry bytes were not restored' "$work/restore-failure"; then
    ok "$description stops before firmware writes and retains the recovery evidence"
  else
    bad "$description did not fail with retained evidence and explicit non-restoration"
  fi
}

# Alter only opaque optional data, then only the EFI device-path bytes.
printf '\001' >> "$work/efivars/Boot000A-$guid"
reject_changed_entry "changed optional data"
cp "$work/original-entry" "$work/efivars/Boot000A-$guid"
printf '\176' | dd of="$work/efivars/Boot000A-$guid" bs=1 seek=14 conv=notrunc status=none
reject_changed_entry "changed device path"
rm "$work/efivars/Boot000A-$guid"
reject_changed_entry "missing foreign entry"
cp "$work/original-entry" "$work/efivars/Boot000A-$guid"

# efibootmgr should only change ordering variables. Detect unexpected entry
# changes during that operation too, rather than reporting successful restore.
cat > "$work/mutating-efibootmgr" <<'EOF'
#!/usr/bin/env bash
printf '\001' >> "$TILO_TEST_MUTATED_ENTRY"
EOF
chmod +x "$work/mutating-efibootmgr"
if TILO_TEST_MUTATED_ENTRY="$work/efivars/Boot000A-$guid" \
    TILO_BOOT_MENU_EFIVARS="$work/efivars" TILO_BOOT_MENU_EFIBOOTMGR="$work/mutating-efibootmgr" \
    "$menu" firmware-restore "$state" > "$work/restore-failure" 2>&1; then
  bad "firmware entry changed during order restoration without detection"
elif cmp -s "$state" "$work/original-state" \
    && grep -Fq 'entry bytes were not restored' "$work/restore-failure"; then
  ok "post-restore verification detects changed entries and retains evidence"
else
  bad "post-restore failure did not retain evidence"
fi

# Execute the actual caller function in a conditional context (which disables
# implicit errexit propagation) without executing any installer disk code.
sed -n '/^  restore_firmware_boot_state() {/,/^  }/p' "$installer" > "$work/caller-function"
if (
  source "$work/caller-function"
  BOOT_MENU=false
  FIRMWARE_STATE="$work/caller-evidence"
  FIRMWARE_RESTORED=0
  cp "$state" "$FIRMWARE_STATE"
  if restore_firmware_boot_state; then exit 1; fi
  [ "$FIRMWARE_RESTORED" -eq 0 ] && cmp -s "$state" "$FIRMWARE_STATE"
); then
  ok "caller retains evidence and failure status even when errexit is suppressed"
else
  bad "caller removed evidence or reported restored state after helper failure"
fi

if grep -Fq 'firmware-snapshot "$FIRMWARE_STATE"' "$installer" \
   && grep -Fq 'firmware-restore "$FIRMWARE_STATE"' "$installer" \
   && grep -Fq '"$BOOT_MENU" install "$BOOT_ESP_ROOT"' "$installer" \
   && grep -Fq 'existing systems remain in the firmware boot picker' "$installer"; then
  ok "alongside installation brackets bootc with firmware-state restore and menu integration"
else
  bad "installer does not invoke the firmware/menu integration"
fi

provisional_line="$(grep -n 'wrote provisional Tilo recovery ownership marker' "$installer" | cut -d: -f1)"
menu_line="$(grep -n 'BOOT_MENU_RESULT=' "$installer" | cut -d: -f1)"
final_line="$(grep -n 'finalized Tilo recovery ownership marker' "$installer" | cut -d: -f1)"
if grep -Fq 'marker_tmp="$(mktemp "$DEPLOY_ETC/tilo/.install-marker.XXXXXX")"' "$installer" \
   && grep -Fq 'mv -f -- "$marker_tmp" "$INSTALL_MARKER"' "$installer" \
   && grep -Fq 'sync -f "$DEPLOY_ETC/tilo"' "$installer" \
   && [ -n "$provisional_line" ] && [ -n "$menu_line" ] && [ -n "$final_line" ] \
   && [ "$provisional_line" -lt "$menu_line" ] && [ "$menu_line" -lt "$final_line" ]; then
  ok "an atomic provisional receipt precedes owned boot entries and is finalized afterwards"
else
  bad "interruption can leave boot entries without an atomic recovery receipt"
fi

exit "$fail"
