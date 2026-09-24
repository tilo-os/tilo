#!/usr/bin/env bash
# The room store volume: sizing policy, the configuration it writes, the boot
# decision it hands fs.service, and the unit wiring that orders the daemon
# after it. Runs from a source tree:
#
#   bash os/tests/store_volume.sh
#
# The unprivileged part always runs. With root (or passwordless sudo) and loop
# devices it also builds an image-backed store on a loop-mounted Btrfs "root"
# and mounts it with the options the installed system uses.
set -uo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
helper="$repo/os/usr/libexec/tilo-store-volume"
fs_unit="$repo/os/usr/lib/systemd/system/fs.service"
volume_unit="$repo/os/usr/lib/systemd/system/tilo-store-volume.service"
loop_unit="$repo/os/usr/lib/systemd/system/tilo-store-loop.service"
containerfile="$repo/os/Containerfile"
fail=0

ok()  { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }
exactly_once() { [ "$(grep -Fxc -- "$2" "$1")" -eq 1 ]; }

GIB=$((1024 * 1024 * 1024))
MIB=$((1024 * 1024))
# ESP 512 MiB + /boot 1 GiB + 1 MiB lead-in + 1 MiB backup GPT.
RESERVED=$((512 * MIB + GIB + 2 * MIB))

tmp="$(mktemp -d)"
cleanup_steps=()
cleanup() {
  local i
  for ((i = ${#cleanup_steps[@]} - 1; i >= 0; i--)); do
    eval "${cleanup_steps[$i]}" >/dev/null 2>&1 || true
  done
  rm -rf --one-file-system "$tmp" 2>/dev/null \
    || run_root rm -rf --one-file-system "$tmp" 2>/dev/null || true
}
trap cleanup EXIT
run_root() { if [ "$(id -u)" -eq 0 ]; then "$@"; else sudo -n "$@"; fi; }

value_of() { printf '%s\n' "$1" | sed -n "s/^$2=//p"; }

# --- whole-disk split ---------------------------------------------------------
check_split() {
  local disk_gib="$1" want_store="$2" out root store usable
  out="$("$helper" plan --disk-bytes $((disk_gib * GIB)) --reserved-bytes "$RESERVED")" \
    || { bad "plan failed for a $disk_gib GiB disk"; return; }
  root="$(value_of "$out" ROOT_BYTES)"
  store="$(value_of "$out" STORE_BYTES)"
  usable=$((disk_gib * GIB - RESERVED))
  if [ $((root + store)) -ne "$usable" ]; then
    bad "$disk_gib GiB: root $root + store $store does not use exactly the $usable usable bytes"
  elif [ "$want_store" = no ] && [ "$store" -ne 0 ]; then
    bad "$disk_gib GiB: a disk this small got a store partition ($store bytes)"
  elif [ "$want_store" = yes ] && [ "$store" -lt $((8 * GIB)) ]; then
    bad "$disk_gib GiB: no store partition of at least 8 GiB ($store bytes)"
  elif [ "$store" -gt 0 ] && [ "$root" -lt $((32 * GIB)) ]; then
    bad "$disk_gib GiB: the root shrank below 32 GiB ($root bytes) to make room for the store"
  elif [ "$store" -gt 0 ] && [ $((root % MIB)) -ne 0 ]; then
    bad "$disk_gib GiB: the root does not end on a MiB boundary ($root bytes)"
  elif [ "$store" -gt 0 ] && [ "$root" -gt $((32 * GIB)) ] && [ $((root - store)) -gt $((2 * MIB)) ]; then
    bad "$disk_gib GiB: a disk with room for both is not split in half (root $root, store $store)"
  else
    ok "$disk_gib GiB disk: root $((root / GIB)) GiB, store $((store / GIB)) GiB"
  fi
}
check_split 32 no
check_split 41 no
check_split 64 yes
check_split 128 yes
check_split 512 yes
check_split 2048 yes

if "$helper" plan --disk-bytes $((10 * GIB)) --reserved-bytes $((20 * GIB)) | grep -qx 'STORE_BYTES=0' \
   && ! "$helper" plan --disk-bytes lots --reserved-bytes 0 >/dev/null 2>&1; then
  ok "plan refuses malformed sizes and handles a disk smaller than its reserve"
else
  bad "plan accepts malformed sizes or mishandles a disk smaller than its reserve"
fi

# --- image on an existing root ------------------------------------------------
check_image() {
  local free_gib="$1" total_gib="$2" want_gib="$3" got
  got="$("$helper" image-size --free-bytes $((free_gib * GIB)) --filesystem-bytes $((total_gib * GIB)) \
    | sed -n 's/^IMAGE_BYTES=//p')"
  if [ "$got" = $((want_gib * GIB)) ]; then
    ok "image on a $total_gib GiB root with $free_gib GiB free: $want_gib GiB"
  else
    bad "image on a $total_gib GiB root with $free_gib GiB free: expected $want_gib GiB, got $got bytes"
  fi
}
check_image 10 100 0      # below the 16 GiB reserve
check_image 30 100 0      # 7 GiB after the reserve: not worth a store
check_image 40 100 12     # (40 - 16) / 2
check_image 200 500 75    # reserve is 10% of 500 GiB
check_image 33 1000 0     # a big, nearly full root keeps its 10%

# --- installer configuration --------------------------------------------------
etc="$tmp/etc"
mkdir -p "$etc/tilo"
printf '# existing comment\nUUID=aaaa / btrfs defaults 0 0\n' > "$etc/fstab"
printf 'other UUID=bbbb none luks\n' > "$etc/crypttab"
head -c 64 /dev/urandom > "$tmp/store.key"
luks=11111111-2222-4333-8444-555555555555
fsu=66666666-7777-4888-9999-aaaaaaaaaaaa
config() {
  "$helper" install-config --etc "$etc" --luks-uuid "$luks" --fs-uuid "$fsu" \
    --key-file "$tmp/store.key" --owner alice >/dev/null
}
if config && config; then
  key="$etc/cryptsetup-keys.d/tilo-store.key"
  if [ "$(stat -c %a "$etc/cryptsetup-keys.d")" = 700 ] && [ "$(stat -c %a "$key")" = 400 ] \
     && cmp -s "$key" "$tmp/store.key"; then
    ok "the store key lands root-only in systemd-cryptsetup's key directory"
  else
    bad "the store key is missing, readable by others or not the formatted key"
  fi
  if exactly_once "$etc/crypttab" "tilo-store UUID=$luks /etc/cryptsetup-keys.d/tilo-store.key luks,discard,no-read-workqueue,no-write-workqueue,nofail" \
     && exactly_once "$etc/crypttab" 'other UUID=bbbb none luks'; then
    ok "crypttab unlocks the store from its key file, once, beside existing entries"
  else
    bad "crypttab lacks the store line, repeats it, or lost an existing entry:"$'\n'"$(cat "$etc/crypttab")"
  fi
  if exactly_once "$etc/fstab" '/dev/mapper/tilo-store /var/lib/tilo/store btrfs noatime,context=system_u:object_r:fusefs_t:s0,nofail,x-systemd.device-timeout=30s 0 0' \
     && exactly_once "$etc/fstab" '# existing comment' \
     && exactly_once "$etc/fstab" 'UUID=aaaa / btrfs defaults 0 0'; then
    ok "fstab mounts the unlocked store nofail at /var/lib/tilo/store, once"
  else
    bad "fstab lacks the store line, repeats it, or lost existing lines:"$'\n'"$(cat "$etc/fstab")"
  fi
  if [ "$(cat "$etc/tilo/store-volume.conf")" = "kind=partition
luks_uuid=$luks
fs_uuid=$fsu
owner=alice" ]; then
    ok "store-volume.conf records the partition and its owner"
  else
    bad "store-volume.conf is not the expected record:"$'\n'"$(cat "$etc/tilo/store-volume.conf")"
  fi
else
  bad "install-config failed"
fi
if ! "$helper" install-config --etc "$etc" --luks-uuid "$luks" --fs-uuid "$fsu" \
       --key-file "$tmp/store.key" --owner 'Robert; rm' >/dev/null 2>&1 \
   && ! "$helper" install-config --etc "$etc" --luks-uuid 'x y' --fs-uuid "$fsu" \
       --key-file "$tmp/store.key" --owner alice >/dev/null 2>&1; then
  ok "install-config refuses malformed owners and UUIDs"
else
  bad "install-config accepts a malformed owner or UUID"
fi

# --- boot decision without a usable store ---------------------------------------
# Every fallback must still tell fs.service to serve FUSE explicitly.
ensure_env() {
  local root="$1"
  TILO_STORE_ETC="$root/etc" TILO_STORE_RUNTIME="$root/run" \
    TILO_STORE_MOUNT="$root/store" TILO_STORE_IMAGE="$root/image-dir/store.img" \
    TILO_STORE_SYSTEMCTL=/bin/false "$helper" ensure > "$root/log" 2>&1
}
me="$(id -un)"
new_root() {
  local root="$tmp/$1"
  mkdir -p "$root/etc/tilo" "$root/etc/greetd" "$root/image-dir"
  printf '[initial_session]\ncommand = "shell"\nuser = "%s"\n' "$2" > "$root/etc/greetd/config.toml"
  printf '%s\n' "$root"
}
fuse_line='TILO_FS_DATA_PLANE_ARGS="--data-plane=fuse"'

r="$(new_root off "$me")"
printf 'kind=none\n' > "$r/etc/tilo/store-volume.conf"
if ensure_env "$r" && [ "$(cat "$r/run/fs.env")" = "$fuse_line" ] && grep -Fq 'kind=none' "$r/log"; then
  ok "kind=none keeps FUSE and says why"
else
  bad "kind=none did not publish FUSE: $(cat "$r/log" 2>/dev/null)"
fi

r="$(new_root nobody-home no-such-person-here)"
if ensure_env "$r" && [ "$(cat "$r/run/fs.env")" = "$fuse_line" ] && grep -Fq 'no local person' "$r/log"; then
  ok "without a local person the store is left alone and FUSE serves"
else
  bad "a store without an owner was not refused: $(cat "$r/log" 2>/dev/null)"
fi

if [ "$(id -u)" -ge 1000 ]; then
  r="$(new_root partition-missing "$me")"
  printf 'kind=partition\n' > "$r/etc/tilo/store-volume.conf"
  if ensure_env "$r" && [ "$(cat "$r/run/fs.env")" = "$fuse_line" ] && grep -Fq 'is not mounted' "$r/log" \
     && [ ! -e "$r/image-dir/store.img" ]; then
    ok "a configured partition that did not mount falls back to FUSE without creating an image"
  else
    bad "a missing store partition was papered over: $(cat "$r/log" 2>/dev/null)"
  fi
fi

# --- unit wiring and image contract --------------------------------------------
if exactly_once "$fs_unit" 'Wants=tilo-store-volume.service var-lib-tilo-store.mount' \
   && exactly_once "$fs_unit" 'After=tilo-store-volume.service var-lib-tilo-store.mount' \
   && exactly_once "$fs_unit" 'Environment=TILO_FS_DATA_PLANE_ARGS=--data-plane=fuse' \
   && exactly_once "$fs_unit" 'EnvironmentFile=-/run/tilo-store/fs.env' \
   && exactly_once "$fs_unit" '    $TILO_FS_DATA_PLANE_ARGS' \
   && exactly_once "$fs_unit" 'ExecStopPost=-/usr/bin/umount --lazy /var/workspace'; then
  ok "fs.service waits for the store and defaults to FUSE unless the store is announced"
else
  bad "fs.service is not ordered after the store or can pick a data plane on its own"
fi
if exactly_once "$volume_unit" 'Before=fs.service' \
   && exactly_once "$volume_unit" 'ExecStart=/usr/libexec/tilo-store-volume ensure' \
   && exactly_once "$volume_unit" 'RuntimeDirectory=tilo-store' \
   && exactly_once "$volume_unit" 'ConditionKernelCommandLine=!tilo.install' \
   && ! grep -q '^WantedBy=' "$volume_unit" \
   && grep -Fq 'install -d -m 0755 "$RUNTIME"' "$helper" \
   && grep -Fq 'RUNTIME="${TILO_STORE_RUNTIME:-/run/tilo-store}"' "$helper"; then
  ok "the store is prepared off the login path, before fs.service, into a root-only runtime directory"
else
  bad "tilo-store-volume.service is not a fs.service prerequisite writing to /run/tilo-store"
fi
if exactly_once "$loop_unit" 'ExecStart=/usr/libexec/tilo-store-volume attach' \
   && exactly_once "$loop_unit" 'ExecStop=/usr/libexec/tilo-store-volume detach' \
   && exactly_once "$loop_unit" 'ConditionPathExists=/var/lib/tilo/store.img' \
   && grep -Fq 'x-systemd.requires=tilo-store-loop.service' "$helper" \
   && grep -Fq -- '--direct-io=on' "$helper"; then
  ok "an image-backed store is attached with direct I/O before its mount and detached after it"
else
  bad "the image-backed store's loop device is not tied to its mount"
fi
if grep -Fq 'os/usr/lib/systemd/system/tilo-store-volume.service \' "$containerfile" \
   && grep -Fq 'os/usr/lib/systemd/system/tilo-store-loop.service \' "$containerfile" \
   && grep -Fq 'os/usr/libexec/tilo-store-volume \' "$containerfile"; then
  ok "the image ships the store helper and both units"
else
  bad "the image does not ship the store helper and its units"
fi

# --- loopback proofs ------------------------------------------------------------
if [ "$(id -u)" -ne 0 ] && ! sudo -n true >/dev/null 2>&1; then
  printf 'SKIP  loopback store proofs need root or passwordless sudo\n'
  [ "$fail" -eq 0 ] || exit 1
  exit 0
fi
for tool in losetup mkfs.btrfs btrfs; do
  command -v "$tool" >/dev/null 2>&1 || { printf 'SKIP  loopback store proofs need %s\n' "$tool"; exit "$fail"; }
done

# fstab options as mount(8) takes them: without systemd's own and, on a host
# without SELinux (where the kernel rejects it), without the context= label.
option_filter="-e ^nofail\$ -e ^x-systemd\\."
if [ ! -e /sys/fs/selinux/enforce ]; then
  option_filter="$option_filter -e ^context="
  printf 'note  this host has no SELinux; the stores mount without their context= label\n'
fi
mount_options() {
  # shellcheck disable=SC2086  # the filter is a list of grep arguments
  printf '%s\n' "$1" | tr ',' '\n' | grep -v $option_filter | paste -sd, -
}

# An existing install: ensure creates, mounts and announces an image store.
truncate -s 64G "$tmp/root.img"
root_loop="$(run_root losetup --find --show "$tmp/root.img")"
cleanup_steps+=("run_root losetup -d '$root_loop'")
run_root mkfs.btrfs -q -f "$root_loop" >/dev/null
r="$tmp/existing"
mkdir -p "$r/fs"
run_root mount "$root_loop" "$r/fs"
cleanup_steps+=("run_root umount -l '$r/fs'")
run_root install -d -m 0755 "$r/fs/etc/tilo" "$r/fs/etc/greetd" "$r/fs/var-lib-tilo" "$r/fs/store"
printf '[initial_session]\ncommand = "shell"\nuser = "%s"\n' "$me" | run_root tee "$r/fs/etc/greetd/config.toml" >/dev/null
# systemctl stand-in: `start <mount>` does what the fstab line does at boot.
cat > "$tmp/systemctl" <<SH
#!/bin/bash
[ "\$1" = start ] || exit 0
export TILO_STORE_IMAGE='$r/fs/var-lib-tilo/store.img'
'$helper' attach >/dev/null || exit 1
line="\$(awk '\$2=="$r/fs/store"' '$r/fs/etc/fstab')"
uuid="\$(printf '%s\n' "\$line" | awk '{print \$1}' | sed 's/^UUID=//')"
opts="\$(printf '%s\n' "\$line" | awk '{print \$4}' | tr ',' '\n' | grep -v $option_filter | paste -sd, -)"
udevadm settle 2>/dev/null || true
dev="\$(losetup -l -n -O NAME -j '$r/fs/var-lib-tilo/store.img' | head -1)"
[ "\$(blkid -s UUID -o value "\$dev")" = "\$uuid" ] || exit 1
mount -t btrfs -o "\$opts" "\$dev" '$r/fs/store'
SH
chmod 0755 "$tmp/systemctl"
cleanup_steps+=("run_root env TILO_STORE_IMAGE='$r/fs/var-lib-tilo/store.img' '$helper' detach")
cleanup_steps+=("run_root umount -l '$r/fs/store'")
if run_root env TILO_STORE_ETC="$r/fs/etc" TILO_STORE_RUNTIME="$r/run" TILO_STORE_MOUNT="$r/fs/store" \
     TILO_STORE_IMAGE="$r/fs/var-lib-tilo/store.img" TILO_STORE_SYSTEMCTL="$tmp/systemctl" \
     "$helper" ensure > "$r/log" 2>&1; then
  ids="$(id -u):$(id -g)"
  image="$r/fs/var-lib-tilo/store.img"
  if [ "$(run_root cat "$r/run/fs.env")" = "TILO_FS_DATA_PLANE_ARGS=\"--data-plane=auto --store=$r/fs/store --native-owner=$ids\"" ] \
     && findmnt -rn -M "$r/fs/store" -o FSTYPE | grep -qx btrfs \
     && run_root lsattr "$image" | awk '{print $1}' | grep -q C \
     && [ "$(run_root stat -c %a "$image")" = 600 ] \
     && [ "$(run_root stat -c %a "$r/fs/store")" = 700 ] \
     && run_root grep -Fq 'x-systemd.requires=tilo-store-loop.service' "$r/fs/etc/fstab" \
     && run_root grep -Fxq 'kind=image' "$r/fs/etc/tilo/store-volume.conf" \
     && run_root grep -Fxq "owner=$me" "$r/fs/etc/tilo/store-volume.conf"; then
    ok "an install without a store gets a no-CoW image store, mounted and announced for $me"
  else
    bad "the image store was not created, mounted or announced as expected:"$'\n'"$(run_root cat "$r/log")"
  fi
  if [ -e /sys/fs/selinux/enforce ]; then
    if run_root stat -c %C "$r/fs/store" | grep -q ':fusefs_t:'; then
      ok "the store carries the label /var/workspace files had under FUSE"
    else
      bad "the store is labelled $(run_root stat -c %C "$r/fs/store"), not fusefs_t"
    fi
  fi
  # A room the daemon created before it knew its owner is handed over.
  run_root btrfs -q subvolume create "$r/fs/store/live/room-a" >/dev/null
  run_root touch "$r/fs/store/live/room-a/file"
  run_root env TILO_STORE_ETC="$r/fs/etc" TILO_STORE_RUNTIME="$r/run" TILO_STORE_MOUNT="$r/fs/store" \
    TILO_STORE_IMAGE="$image" TILO_STORE_SYSTEMCTL=/bin/false "$helper" ensure > "$r/log2" 2>&1
  if [ "$(run_root stat -c %u:%g "$r/fs/store/live/room-a/file")" = "$ids" ] \
     && [ "$(run_root ls "$r/fs/var-lib-tilo")" = store.img ]; then
    ok "the next boot reuses the image and hands root-owned rooms to the local person"
  else
    bad "a second boot did not reuse the image or left a room with root:"$'\n'"$(run_root cat "$r/log2")"
  fi
else
  bad "ensure failed on an install without a store:"$'\n'"$(run_root cat "$r/log")"
fi

[ "$fail" -eq 0 ] || exit 1
echo "store volume: all checks passed"
