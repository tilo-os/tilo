#!/usr/bin/env bash
# UEFI-like filesystem fixtures for alongside boot-menu integration.
set -uo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
menu="$repo/os/usr/libexec/qos-install-boot-menu"
artifacts="$repo/os/usr/libexec/qos-install-efi-artifacts"
installer="$repo/os/usr/libexec/qos-install"
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
output="$(QOS_BOOT_MENU_ARCH=x86_64 "$menu" install "$esp" "$uuid")" || bad "boot menu fixture installs"
if grep -Fxq loader/entries/qos-windows.conf <<<"$output" \
   && grep -Fxq loader/entries/qos-linux-ubuntu.conf <<<"$output" \
   && grep -Fxq loader/entries/qos-linux-fedora.conf <<<"$output" \
   && grep -Fqx 'efi /EFI/Microsoft/Boot/bootmgfw.efi' "$esp/loader/entries/qos-windows.conf" \
   && grep -Fqx 'efi /EFI/ubuntu/shimx64.efi' "$esp/loader/entries/qos-linux-ubuntu.conf" \
   && grep -Fqx 'efi /EFI/fedora/grubx64.efi' "$esp/loader/entries/qos-linux-fedora.conf"; then
  ok "Windows and Linux loaders get deterministic same-ESP systemd-boot entries"
else
  bad "Windows/Linux entry contents are incomplete"
fi

arm="$work/arm"
mkdir -p "$arm/EFI/debian"
printf armshim > "$arm/EFI/debian/shimaa64.efi"
arm_output="$(QOS_BOOT_MENU_ARCH=aarch64 "$menu" install "$arm" "$uuid")"
if grep -Fxq loader/entries/qos-linux-debian.conf <<<"$arm_output" \
   && grep -Fqx 'efi /EFI/debian/shimaa64.efi' "$arm/loader/entries/qos-linux-debian.conf"; then
  ok "ARM64 Linux shim discovery uses the shipped systemd-boot efi-entry syntax"
else
  bad "ARM64 loader discovery or entry syntax is wrong"
fi

owned="$($artifacts "$esp" "$uuid")"
if grep -Fxq loader/entries/qos-windows.conf <<<"$owned" \
   && grep -Fxq loader/entries/qos-linux-ubuntu.conf <<<"$owned" \
   && grep -Fxq loader/entries/qos-linux-fedora.conf <<<"$owned"; then
  ok "chainloader entries are bound to the qOS root ownership receipt"
else
  bad "chainloader entries are not discoverable for owned cleanup"
fi

# Removal consumes exactly the ownership helper's paths. Exercise that contract
# on a FAT-like tree and prove the foreign EFI programs remain byte-for-byte.
before="$(sha256sum "$esp/EFI/Microsoft/Boot/bootmgfw.efi" "$esp/EFI/ubuntu/shimx64.efi" "$esp/EFI/fedora/grubx64.efi")"
while IFS= read -r relative; do [ -z "$relative" ] || rm -f -- "$esp/$relative"; done <<<"$owned"
after="$(sha256sum "$esp/EFI/Microsoft/Boot/bootmgfw.efi" "$esp/EFI/ubuntu/shimx64.efi" "$esp/EFI/fedora/grubx64.efi")"
if [ "$before" = "$after" ] && [ -f "$esp/EFI/Microsoft/Boot/bootmgfw.efi" ] \
   && [ ! -e "$esp/loader/entries/qos-windows.conf" ]; then
  ok "owned-entry removal leaves every foreign EFI binary untouched"
else
  bad "cleanup changed a foreign EFI loader or retained an owned entry"
fi

ambiguous="$work/ambiguous"
mkdir -p "$ambiguous/EFI/Microsoft/Boot" "$ambiguous/EFI/microsoft/boot"
printf one > "$ambiguous/EFI/Microsoft/Boot/bootmgfw.efi"
printf two > "$ambiguous/EFI/microsoft/boot/bootmgfw.efi"
if QOS_BOOT_MENU_ARCH=x86_64 "$menu" install "$ambiguous" "$uuid" >"$work/ambiguous.out" 2>&1; then
  bad "ambiguous Windows loaders were accepted"
elif [ ! -e "$ambiguous/loader/entries/qos-windows.conf" ]; then
  ok "ambiguous loader discovery fails before writing an entry"
else
  bad "ambiguous discovery left a partial boot-menu entry"
fi

symlinked="$work/symlinked"
mkdir -p "$symlinked/EFI/ubuntu" "$work/outside"
printf shim > "$symlinked/EFI/ubuntu/shimx64.efi"
ln -s "$work/outside" "$symlinked/loader"
if QOS_BOOT_MENU_ARCH=x86_64 "$menu" install "$symlinked" "$uuid" >"$work/symlink.out" 2>&1; then
  bad "symlinked loader entry directory was accepted"
elif [ ! -e "$work/outside/entries/qos-linux-ubuntu.conf" ]; then
  ok "symlinked output directories fail closed without traversal"
else
  bad "boot entry escaped through a symlink"
fi

owned_conf="$work/foreign-owned/EFI/ubuntu"
mkdir -p "$owned_conf" "$work/foreign-owned/loader/entries"
printf shim > "$owned_conf/shimx64.efi"
printf 'title foreign\n' > "$work/foreign-owned/loader/entries/qos-linux-ubuntu.conf"
if QOS_BOOT_MENU_ARCH=x86_64 "$menu" install "$work/foreign-owned" "$uuid" >"$work/foreign.out" 2>&1; then
  bad "a pre-existing foreign qOS-prefixed filename was overwritten"
else
  ok "pre-existing entries require the exact qOS root owner marker"
fi

# efibootmgr is injected here so the fixture can prove that bootc-created
# ordering/default changes are rolled back exactly, without touching host NVRAM.
mkdir -p "$work/efivars"
fake="$work/efibootmgr"
cat > "$fake" <<'EOF'
#!/usr/bin/env bash
if [ "$#" -eq 0 ]; then
  printf 'BootCurrent: 0002\nBootNext: 0003\nBootOrder: 0002,0000,0003\n'
else
  printf '%s\n' "$*" >> "$QOS_FAKE_EFIBOOT_LOG"
fi
EOF
chmod +x "$fake"
state="$work/firmware.state"
export QOS_FAKE_EFIBOOT_LOG="$work/efiboot.log"
QOS_BOOT_MENU_EFIVARS="$work/efivars" QOS_BOOT_MENU_EFIBOOTMGR="$fake" \
  "$menu" firmware-snapshot "$state"
QOS_BOOT_MENU_EFIVARS="$work/efivars" QOS_BOOT_MENU_EFIBOOTMGR="$fake" \
  "$menu" firmware-restore "$state"
if grep -Fxq -- '-o 0002,0000,0003' "$QOS_FAKE_EFIBOOT_LOG" \
   && grep -Fxq -- '-n 0003' "$QOS_FAKE_EFIBOOT_LOG"; then
  ok "firmware BootOrder and BootNext/default are restored exactly"
else
  bad "firmware state was not faithfully restored"
fi

if grep -Fq 'firmware-snapshot "$FIRMWARE_STATE"' "$installer" \
   && grep -Fq 'firmware-restore "$FIRMWARE_STATE"' "$installer" \
   && grep -Fq '"$BOOT_MENU" install "$BOOT_ESP_ROOT"' "$installer" \
   && grep -Fq 'existing systems remain in the firmware boot picker' "$installer"; then
  ok "alongside installation brackets bootc with firmware-state restore and menu integration"
else
  bad "installer does not invoke the firmware/menu integration"
fi

exit "$fail"
