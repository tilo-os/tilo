#!/usr/bin/env bash
# The room store volume: sizing policy, the configuration it writes, the boot
# decision it hands fs.service, and the unit wiring that orders the daemon
# after it. Runs from a source tree:
#
#   bash os/tests/store_volume.sh
#
# The unprivileged part always runs. With root (or passwordless sudo) and loop
# devices it also builds a whole-disk encrypted layout on a sparse loop disk
# and an image-backed store on a loop-mounted Btrfs "root", and mounts both
# with the options the installed system uses.
set -uo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
helper="$repo/os/usr/libexec/tilo-store-volume"
encryptor="$repo/os/usr/libexec/tilo-disk-encrypt"
backend="$repo/os/usr/libexec/tilo-install"
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

# --- who the local person is ----------------------------------------------------
# A device with several accounts: the person is the account holding the
# identity the device is bound to, never the greeter's configuration or an
# account count.
ensure_env() {
  local root="$1"
  TILO_STORE_ETC="$root/etc" TILO_STORE_RUNTIME="$root/run" \
    TILO_STORE_MOUNT="$root/store" TILO_STORE_IMAGE="$root/image-dir/store.img" \
    TILO_STORE_IDENTITY="$root/identity" TILO_STORE_PASSWD="$root/passwd" \
    TILO_STORE_LOGINCTL="${LOGINCTL_STUB:-/bin/false}" \
    TILO_STORE_SYSTEMCTL=/bin/false "$helper" ensure > "$root/log" 2>&1
}
me="$(id -un)"
device_id=61fd5d11de98fe19ddba9ae2149dd14abb8c3c8c72473bb787b0f1552f751241
other_id=0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f
# A root with accounts ada (1000), bob (1001), cy (1002) and dee (1003), the
# device bound to $device_id; each argument after the first is
# "account:identity" for an account whose registry holds that identity.
new_root() {
  local root="$tmp/$1" spec name id uid=1000
  shift
  mkdir -p "$root/etc/tilo" "$root/image-dir" "$root/identity/user-identity" "$root/home"
  printf '%s' "$device_id" | sed 's/../\\x&/g' | xargs -0 printf '%b' > "$root/identity/user-identity/pubkey.bin"
  : > "$root/passwd"
  for name in ada bob cy dee; do
    printf '%s:x:%s:%s::%s:/bin/bash\n' "$name" "$uid" "$uid" "$root/home/$name" >> "$root/passwd"
    mkdir -p "$root/home/$name/.config/tilo/identity"
    uid=$((uid + 1))
  done
  printf 'svc:x:990:990::%s:/sbin/nologin\n' "$root/home/svc" >> "$root/passwd"
  for spec in "$@"; do
    name="${spec%%:*}" id="${spec#*:}"
    mkdir -p "$root/home/$name/.config/tilo/identity"
    printf '{\n  "default_identity": "%s",\n  "identities": {\n    "%s": {\n      "id": "%s"\n    }\n  }\n}\n' \
      "$id" "$id" "$id" > "$root/home/$name/.config/tilo/identity/registry.json"
  done
  printf '%s\n' "$root"
}
fuse_for() { printf 'TILO_FS_DATA_PLANE_ARGS="--data-plane=fuse --native-owner=%s"' "$1"; }
fuse_line='TILO_FS_DATA_PLANE_ARGS="--data-plane=fuse"'

r="$(new_root bound "bob:$other_id" "cy:$device_id" "dee:$other_id")"
printf 'kind=none\n' > "$r/etc/tilo/store-volume.conf"
if ensure_env "$r" && [ "$(cat "$r/run/fs.env")" = "$(fuse_for 1002:1002)" ] && grep -Fq 'kind=none' "$r/log"; then
  ok "of four accounts, the one holding the device's identity owns the files (FUSE, kind=none)"
else
  bad "the device's bound person was not chosen: $(cat "$r/run/fs.env" 2>/dev/null) $(cat "$r/log" 2>/dev/null)"
fi

r="$(new_root configured "cy:$device_id")"
printf 'kind=none\nowner=dee\n' > "$r/etc/tilo/store-volume.conf"
if ensure_env "$r" && [ "$(cat "$r/run/fs.env")" = "$(fuse_for 1003:1003)" ]; then
  ok "owner= in store-volume.conf names the person outright"
else
  bad "owner= was not honoured: $(cat "$r/run/fs.env" 2>/dev/null) $(cat "$r/log" 2>/dev/null)"
fi

r="$(new_root ambiguous "ada:$device_id" "bob:$device_id")"
printf 'kind=none\n' > "$r/etc/tilo/store-volume.conf"
if ! ensure_env "$r" && [ "$(cat "$r/run/fs.env")" = "$fuse_line" ] \
   && grep -Fq 'Several accounts hold this device' "$r/log" && grep -Fq 'owner=<account>' "$r/log"; then
  ok "two accounts claiming the device's identity fail loudly and say what to configure"
else
  bad "an ambiguous person was guessed: $(cat "$r/run/fs.env" 2>/dev/null) $(cat "$r/log" 2>/dev/null)"
fi

cat > "$tmp/loginctl" <<'SH'
#!/bin/bash
case "$1 $2" in
  "show-seat seat0") echo c7 ;;
  "show-session c7") echo bob ;;
  *) exit 1 ;;
esac
SH
chmod 0755 "$tmp/loginctl"
r="$(new_root signed-in "ada:$other_id")"
printf 'kind=none\n' > "$r/etc/tilo/store-volume.conf"
if LOGINCTL_STUB="$tmp/loginctl" ensure_env "$r" && [ "$(cat "$r/run/fs.env")" = "$(fuse_for 1001:1001)" ]; then
  ok "with no account bound, the person signed in at the login screen owns the files"
else
  bad "the signed-in person was not chosen: $(cat "$r/run/fs.env" 2>/dev/null) $(cat "$r/log" 2>/dev/null)"
fi

r="$(new_root nobody "ada:$other_id" "bob:$other_id")"
if ! ensure_env "$r" && [ "$(cat "$r/run/fs.env")" = "$fuse_line" ] \
   && grep -Fq 'no local person' "$r/log" && grep -Fq "owner=<account> in $r/etc/tilo/store-volume.conf" "$r/log" \
   && [ ! -e "$r/image-dir/store.img" ]; then
  ok "with no person found the unit fails loudly, says what to configure, and FUSE still serves"
else
  bad "a device without a person was not refused loudly: $(cat "$r/log" 2>/dev/null)"
fi

if [ "$(id -u)" -ge 1000 ]; then
  r="$(new_root partition-missing)"
  printf 'kind=partition\nowner=ada\n' > "$r/etc/tilo/store-volume.conf"
  if ensure_env "$r" && [ "$(cat "$r/run/fs.env")" = "$(fuse_for 1000:1000)" ] && grep -Fq 'is not mounted' "$r/log" \
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

# The installer's whole-disk layout and the recovery-key binding.
if grep -Fq 'GUID_TILO_STORE=0B6C5D38-8BA9-40AA-A1F6-DCC035F5438D' "$encryptor" \
   && grep -Fq 'size=$((root_bytes / 1048576))MiB, type=$GUID_TILO_ROOT, name="Tilo root"' "$encryptor" \
   && grep -Fq 'type=$GUID_TILO_STORE, name="Tilo store"' "$encryptor" \
   && ! grep -qi '0b6c5d38-8ba9-40aa-a1f6-dcc035f5438d' "$repo/os/usr/libexec/tilo-root-discover" \
   && grep -Fq -- '--new-keyfile "$scratch/recovery" "$store_part"' "$encryptor" \
   && grep -Fq 'ENROL_ARGS+=(--store-part "$STORE_PART" --store-key-file "$STORE_KEY_FILE")' "$backend" \
   && grep -Fq '"$STORE_VOLUME" install-config --etc "$DEPLOY_ETC"' "$backend"; then
  ok "whole-disk installs end with a Tilo-typed store partition the recovery key also opens"
else
  bad "the installer's store partition, its type or its recovery binding is missing"
fi

# --- loopback proofs ------------------------------------------------------------
if [ "$(id -u)" -ne 0 ] && ! sudo -n true >/dev/null 2>&1; then
  printf 'SKIP  loopback store proofs need root or passwordless sudo\n'
  [ "$fail" -eq 0 ] || exit 1
  exit 0
fi
for tool in losetup cryptsetup systemd-cryptenroll mkfs.btrfs sfdisk btrfs; do
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

# 1. Whole disk: prepare, enrol, configure, finish.
truncate -s 64G "$tmp/disk.img"
if ! disk="$(run_root losetup --find --show --partscan "$tmp/disk.img" 2>"$tmp/losetup.err")"; then
  printf 'SKIP  loopback store proofs need a usable loop device (%s)\n' "$(tr '\n' ' ' < "$tmp/losetup.err")"
  exit "$fail"
fi
cleanup_steps+=("run_root losetup -d '$disk'")
install -d -m 0700 "$tmp/keys"
# A loop device is not TYPE disk; wrap only that one lsblk query so prepare's
# whole-disk gate stays exercised.
mkdir -p "$tmp/bin"
cat > "$tmp/bin/lsblk" <<SH
#!/bin/sh
if [ "\$#" -eq 3 ] && [ "\$1" = -dno ] && [ "\$2" = TYPE ] && [ "\$3" = '$disk' ]; then
  echo disk
else
  exec /usr/bin/lsblk "\$@"
fi
SH
chmod 0755 "$tmp/bin/lsblk"
cleanup_steps+=("run_root env PATH='$tmp/bin:$PATH' '$encryptor' teardown '$disk' --mnt '$tmp/mnt'")
if prep="$(run_root env PATH="$tmp/bin:$PATH" TILO_STORE_VOLUME="$helper" "$encryptor" prepare "$disk" \
             --mnt "$tmp/mnt" --key-file "$tmp/keys/luks.key" 2>"$tmp/prepare.err")"; then
  store_part="$(value_of "$prep" STORE_PART)"
  store_key="$(value_of "$prep" STORE_KEY_FILE)"
  luks_part="$(value_of "$prep" LUKS_PART)"
  types="$(run_root sfdisk --json "$disk" | python3 -c 'import json,sys; print(" ".join(p["type"].lower() for p in json.load(sys.stdin)["partitiontable"]["partitions"]))')"
  root_size="$(run_root blockdev --getsize64 "$luks_part")"
  want_root="$(value_of "$("$helper" plan --disk-bytes $((64 * GIB)) --reserved-bytes "$RESERVED")" ROOT_BYTES)"
  if [ "$types" = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b 0fc63daf-8483-4772-8e79-3d69d8477de4 7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab 0b6c5d38-8ba9-40aa-a1f6-dcc035f5438d" ] \
     && [ "$root_size" = "$want_root" ] && [ "$store_key" = "$tmp/keys/store.key" ] \
     && run_root cryptsetup isLuks --type luks2 "$store_part"; then
    ok "a 64 GiB disk gets ESP, /boot, a $((root_size / GIB)) GiB LUKS2 root and a LUKS2 store"
  else
    bad "unexpected whole-disk layout: types [$types], root $root_size (planned $want_root), key $store_key"
  fi

  printf '%s' 'correct horse battery staple' > "$tmp/keys/pin"
  if enrol_out="$(run_root "$encryptor" enrol "$luks_part" --key-file "$tmp/keys/luks.key" \
        --passphrase --pin-file "$tmp/keys/pin" \
        --store-part "$store_part" --store-key-file "$store_key" 2>&1)"; then
    recovery="$(printf '%s\n' "$enrol_out" | sed -n 's/^::tilo-recovery-key:://p' | tail -1)"
    printf '%s' "$recovery" > "$tmp/keys/recovery"
    if [ -n "$recovery" ] \
       && run_root cryptsetup open --test-passphrase --key-file "$tmp/keys/recovery" "$store_part" \
       && run_root cryptsetup open --test-passphrase --key-file "$store_key" "$store_part" \
       && ! run_root cryptsetup open --test-passphrase --key-file "$tmp/keys/pin" "$store_part" 2>/dev/null; then
      ok "the install's recovery key and the root's key file open the store; the boot passphrase does not"
    else
      bad "the store does not open with exactly the recovery key and its key file"
    fi
  else
    bad "enrol with a store volume failed:"$'\n'"$enrol_out"
  fi

  deploy_etc="$tmp/mnt/etc-under-test"
  run_root install -d -m 0755 "$deploy_etc"
  if run_root "$helper" install-config --etc "$deploy_etc" \
       --luks-uuid "$(value_of "$prep" STORE_LUKS_UUID)" --fs-uuid "$(value_of "$prep" STORE_FS_UUID)" \
       --key-file "$store_key" --owner "$me" >/dev/null; then
    # What systemd-cryptsetup and the fstab line do at boot, by hand.
    run_root cryptsetup open --key-file "$deploy_etc/cryptsetup-keys.d/tilo-store.key" "$store_part" tilo-store-under-test
    cleanup_steps+=("run_root cryptsetup close tilo-store-under-test")
    shipped="$(run_root awk '$2=="/var/lib/tilo/store"{print $4}' "$deploy_etc/fstab")"
    case "$shipped" in
      *noatime*context=*) ;;
      *) bad "the installed fstab line carries no usable mount options ('$shipped')" ;;
    esac
    options="$(mount_options "$shipped")"
    mkdir -p "$tmp/store-mnt"
    if run_root mount -t btrfs -o "$options" /dev/mapper/tilo-store-under-test "$tmp/store-mnt"; then
      cleanup_steps+=("run_root umount '$tmp/store-mnt'")
      features="$(run_root btrfs inspect-internal dump-super /dev/mapper/tilo-store-under-test)"
      if grep -q 'BLOCK_GROUP_TREE' <<<"$features" && grep -q 'SIMPLE_QUOTA' <<<"$features" \
         && [ "$(run_root blkid -s LABEL -o value /dev/mapper/tilo-store-under-test)" = tilo-store ]; then
        ok "the store unlocks from the installed key file and mounts with the shipped options"
      else
        bad "the store filesystem lacks its label, block-group tree or simple quotas"
      fi
      run_root umount "$tmp/store-mnt"
    else
      bad "the store does not mount with the shipped options ($options)"
    fi
    run_root cryptsetup close tilo-store-under-test
  else
    bad "install-config failed on the formatted store"
  fi

  if run_root "$encryptor" finish "$luks_part" --key-file "$tmp/keys/luks.key" --mnt "$tmp/mnt" >/dev/null 2>&1 \
     && [ ! -e "$store_key" ] && [ ! -e "$tmp/keys/luks.key" ]; then
    ok "finish removes both install-time key files from the live medium"
  else
    bad "finish left an install-time key file behind"
  fi
else
  bad "prepare failed:"$'\n'"$(cat "$tmp/prepare.err")"
fi

# 2. An existing install: ensure creates, mounts and announces an image store.
truncate -s 64G "$tmp/root.img"
root_loop="$(run_root losetup --find --show "$tmp/root.img")"
cleanup_steps+=("run_root losetup -d '$root_loop'")
run_root mkfs.btrfs -q -f "$root_loop" >/dev/null
r="$tmp/existing"
mkdir -p "$r/fs"
run_root mount "$root_loop" "$r/fs"
cleanup_steps+=("run_root umount -l '$r/fs'")
run_root install -d -m 0755 "$r/fs/etc/tilo" "$r/fs/var-lib-tilo" "$r/fs/store"
printf 'owner=%s\n' "$me" | run_root tee "$r/fs/etc/tilo/store-volume.conf" >/dev/null
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
