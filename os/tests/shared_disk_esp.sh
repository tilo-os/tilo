#!/usr/bin/bash
# A Tilo install that shares its disk with another operating system, as on a
# laptop that came with Windows. Run IN a booted guest, as root, on a throwaway
# disk:
#
#   shared_disk_esp.sh hold-size      keep the root partition from growing
#   (power off, grow the disk image, boot)
#   shared_disk_esp.sh prepare        add a foreign ESP ahead of this system's
#   (reboot)
#   shared_disk_esp.sh check IMAGE    bootc status, then `bootc switch IMAGE`
#
# Windows' ESP is usually the disk's first partition and Tilo's comes later.
# bootc used to take the first ESP on the root's disk, so on such a machine
# `bootc status`, `upgrade` and `switch` all failed reading loader entries the
# foreign ESP does not have. `prepare` reproduces that layout. It needs free
# space at the end of the disk, which a grown disk image keeps only when
# `hold-size` stopped the boot from growing the root into it. It creates a FAT
# ESP with only a Windows boot manager path in it and gives it this system's
# ESP's partition number, so it comes first. The firmware still starts systemd-boot
# from this system's ESP through the removable-media path, which the foreign
# ESP lacks. `check` fails unless the layout is really in place, so it cannot
# pass on a disk with a single ESP.
set -euo pipefail

ESP_TYPE=c12a7328-f81f-11d2-ba4b-00a0c93ec93b
LOADER_VAR=/sys/firmware/efi/efivars/LoaderDevicePartUUID-4a67b082-0a4c-41cf-b6c7-440b29bb8c4f
FOREIGN_LABEL=SYSTEM_DRV

die() { printf 'FAIL  %s\n' "$*" >&2; exit 1; }
ok() { printf 'ok    %s\n' "$*"; }

[ "$(id -u)" = 0 ] || die "needs root"

# The partition UUID the boot loader was started from, in lower case.
loader_partuuid() {
  [ -r "$LOADER_VAR" ] || die "no LoaderDevicePartUUID: not booted through systemd-boot"
  tail -c +5 "$LOADER_VAR" | tr -d '\0' | tr '[:upper:]' '[:lower:]'
}

loader_esp() {
  local uuid dev
  uuid="$(loader_partuuid)"
  dev="$(lsblk -nrpo NAME,PARTUUID | awk -v u="$uuid" 'tolower($2) == u { print $1; exit }')"
  [ -n "$dev" ] || die "no partition carries the loader's partition UUID $uuid"
  printf '%s\n' "$dev"
}

disk_of() {
  printf '/dev/%s\n' "$(lsblk -ndo PKNAME "$1")"
}

partnum() {
  cat "/sys/class/block/${1##*/}/partition"
}

# The ESPs on DISK in partition-number order: NUMBER NAME PARTUUID LABEL.
esps_of() {
  local name type uuid label
  lsblk -nrpo NAME,PARTTYPE,PARTUUID,LABEL "$1" | while read -r name type uuid label; do
    [ "${type,,}" = "$ESP_TYPE" ] || continue
    printf '%s %s %s %s\n' "$(partnum "$name")" "$name" "${uuid,,}" "$label"
  done | sort -n
}

# The boot grows the root partition and filesystem into free space at the end
# of the disk; the foreign ESP needs that space.
cmd_hold_size() {
  systemctl mask bootc-generic-growpart.service systemd-growfs-root.service \
    tilo-growfs-sysroot.service tilo-growfs-sysroot.timer
  ok "the root partition keeps its size from the next boot"
}

cmd_prepare() {
  local esp disk own new dump node
  esp="$(loader_esp)"
  disk="$(disk_of "$esp")"
  own="$(partnum "$esp")"
  if esps_of "$disk" | awk -v l="$FOREIGN_LABEL" '$4 == l { found = 1 } END { exit !found }'; then
    die "$disk already has a foreign ESP"
  fi
  # The disk image was grown: move the backup GPT to its new end, then put the
  # foreign ESP in the space that freed.
  sfdisk --relocate gpt-bak-std "$disk"
  printf 'size=64MiB, type=%s, name="EFI system partition"\n' "${ESP_TYPE^^}" \
    | sfdisk --append --no-reread --no-tell-kernel "$disk"
  new="$(sfdisk -d "$disk" | sed -n "s|^${disk}p\{0,1\}\([0-9]\{1,\}\) :.*|\1|p" | sort -n | tail -1)"
  [ -n "$new" ] && [ "$new" != "$own" ] || die "could not find the new partition on $disk"
  # Only the new partition is free to be added to the running kernel's view.
  partx -a --nr "$new" "$disk"
  node="$(lsblk -nrpo NAME "$disk" | while read -r n; do
    [ "$n" != "$disk" ] && [ "$(partnum "$n")" = "$new" ] && echo "$n"; done)"
  [ -b "$node" ] || die "partition $new of $disk did not appear"
  mkfs.vfat -F 32 -n "$FOREIGN_LABEL" "$node" >/dev/null
  local mnt
  mnt="$(mktemp -d)"
  mount "$node" "$mnt"
  mkdir -p "$mnt/EFI/Microsoft/Boot"
  printf 'not a boot manager\n' > "$mnt/EFI/Microsoft/Boot/bootmgfw.efi"
  umount "$mnt"
  rmdir "$mnt"
  partx -d --nr "$new" "$disk"
  # Swap the two entries' numbers: the foreign ESP takes this system's, which
  # is the lower one. The entries' contents and UUIDs are unchanged.
  dump="$(sfdisk -d "$disk")"
  dump="$(printf '%s\n' "$dump" | sed \
    -e "s|^\(${disk}p\{0,1\}\)${own} :|\1@OWN@ :|" \
    -e "s|^\(${disk}p\{0,1\}\)${new} :|\1${own} :|" \
    -e "s|@OWN@ :|${new} :|")"
  printf '%s\n' "$dump" | sfdisk --no-reread --no-tell-kernel --force "$disk" >/dev/null
  sfdisk -d "$disk"
  ok "foreign ESP is partition $own of $disk, this system's ESP is now $new; reboot to use it"
}

cmd_check() {
  local target="${1:-}" esp disk first status
  [ -n "$target" ] || die "usage: $0 check IMAGE"
  esp="$(loader_esp)"
  disk="$(disk_of "$esp")"
  esps_of "$disk"
  first="$(esps_of "$disk" | head -1)"
  set -- $first
  [ "${4:-}" = "$FOREIGN_LABEL" ] && [ "$2" != "$esp" ] \
    || die "the first ESP on $disk is not the foreign one; run prepare and reboot first"
  ok "first ESP $2 is foreign; systemd-boot started from $esp"

  status="$(bootc status --json)" || die "bootc status failed on a disk with a foreign first ESP"
  python3 -c 'import json, sys; s = json.load(sys.stdin)["status"]; assert s["booted"]["image"], s' \
    <<< "$status" || die "bootc status reports no booted image"
  ok "bootc status"

  # The ESP is mounted read-only; staging writes the new entry onto it.
  if findmnt -n /boot >/dev/null; then mount -o remount,rw /boot; fi
  timeout --kill-after=10s 25m bootc switch "$target" || die "bootc switch $target failed"
  status="$(bootc status --json)" || die "bootc status failed after the switch"
  python3 -c '
import json, sys
target = sys.argv[1]
staged = json.load(sys.stdin)["status"].get("staged")
assert staged, "nothing staged"
image = staged["image"]["image"]["image"]
assert image == target, f"staged {image}, not {target}"
' "$target" <<< "$status" || die "bootc switch staged no deployment of $target"
  ok "bootc switch staged $target"
  # The staged entry went to this system's ESP, never to the foreign one.
  local mnt
  mnt="$(mktemp -d)"
  mount -o ro "$(esps_of "$disk" | awk 'NR == 1 { print $2 }')" "$mnt"
  if [ -e "$mnt/loader" ] || [ -e "$mnt/EFI/Linux" ]; then
    umount "$mnt"; rmdir "$mnt"
    die "bootc wrote to the foreign ESP"
  fi
  umount "$mnt"
  rmdir "$mnt"
  ok "the foreign ESP is untouched"
}

case "${1:-}" in
  hold-size) cmd_hold_size ;;
  prepare) cmd_prepare ;;
  check) shift; cmd_check "$@" ;;
  *) die "usage: $0 hold-size | prepare | check IMAGE" ;;
esac
