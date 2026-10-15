#!/usr/bin/env bash
# Destructive loopback proof for USB-free installation (design spec §6/§7).
# Real util-linux output drives tilo-staging-handoff's same-disk guard, and a
# real GPT on a loop device is reclaimed by tilo-staging-reclaim: the staging
# partitions disappear, the adjacent Tilo root partition and its btrfs grow
# into their space, and every other partition keeps its exact geometry.
#
# Needs root and loop devices (CI disk runners). A run that cannot observe the
# real disk path fails instead of skipping: the unprivileged fixtures in the
# two crates' tests cannot stand in for it.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# The handoff guard and the reclaim logic are compiled
# (crates/system/install-helpers, crates/system/staging-reclaim); their fixture
# examples take the firmware variables, ESP and lsblk output, and the record,
# root mount and loop allowance, from the environment, which the shipped
# binaries never do.
cargo build --quiet --locked --manifest-path "$repo/Cargo.toml" -p tilo-install-helpers \
  --example staging-handoff-fixture \
  || { echo 'FAIL  cannot build the staging handoff fixture' >&2; exit 1; }
handoff_helper="${CARGO_TARGET_DIR:-$repo/target}/debug/examples/staging-handoff-fixture"
cargo build --quiet --locked --manifest-path "$repo/Cargo.toml" -p tilo-staging-reclaim \
  --example staging-reclaim-fixture \
  || { echo 'FAIL  cannot build the staging reclaim fixture' >&2; exit 1; }
reclaim="${CARGO_TARGET_DIR:-$repo/target}/debug/examples/staging-reclaim-fixture"

if [ "$(id -u)" -ne 0 ]; then
  echo 'FAIL  staging reclaim loopback proof must run as root (it cannot observe a real GPT otherwise)' >&2
  exit 1
fi
for tool in losetup sfdisk partx lsblk mkfs.btrfs btrfs findmnt python3 truncate mknod stat; do
  command -v "$tool" >/dev/null 2>&1 || { echo "FAIL  missing required tool: $tool" >&2; exit 1; }
done

tmp="$(mktemp -d)"
loop=""
mnt="$tmp/root"

# The kernel registers a partition in sysfs before `partx -u` returns. A
# devtmpfs /dev shows its node at the same moment, but a container's /dev can
# be a static snapshot that never does, and a partition number the table reuses
# can leave a node with another partition's device number. Make /dev match
# sysfs for this loop device: create or replace each partition's node, and
# remove nodes whose partition is gone. On a devtmpfs /dev this changes nothing.
sync_partition_nodes() {
  local name="${loop##*/}" sys node want have major minor
  for sys in /sys/class/block/"$name"/"$name"p*; do
    [ -r "$sys/dev" ] || continue
    node="/dev/${sys##*/}"
    want="$(cat "$sys/dev")"
    have=""
    if [ -b "$node" ]; then
      read -r major minor < <(stat -c '%t %T' "$node")
      have="$((16#$major)):$((16#$minor))"
    fi
    [ "$have" = "$want" ] && continue
    rm -f "$node"
    mknod "$node" b "${want%%:*}" "${want##*:}" \
      || { echo "FAIL  cannot create the partition node $node ($want)" >&2; exit 1; }
  done
  for node in /dev/"$name"p*; do
    [ -e "$node" ] || continue
    [ -e "/sys/class/block/$name/${node##*/}" ] || rm -f "$node"
  done
}

cleanup() {
  umount "$mnt" 2>/dev/null || true
  if [ -n "$loop" ]; then
    losetup -d "$loop" 2>/dev/null || true
    # Nodes this run made in a static /dev outlive the detached device.
    sync_partition_nodes 2>/dev/null || true
  fi
  rm -rf "$tmp"
}
trap cleanup EXIT

truncate -s 16G "$tmp/disk.img"
loop="$(losetup --find --show --partscan "$tmp/disk.img")" \
  || { echo 'FAIL  could not attach a loop device' >&2; exit 1; }

disk_guid=11111111-2222-4333-8444-555555555555
esp_uuid=aaaaaaaa-0000-4000-8000-000000000001
root_uuid=bbbbbbbb-0000-4000-8000-000000000002
target_esp_uuid=cccccccc-0000-4000-8000-000000000003
target_boot_uuid=dddddddd-0000-4000-8000-000000000004
target_root_uuid=eeeeeeee-0000-4000-8000-000000000005
recovery_uuid=00000000-0000-4000-8000-00000000000d
mib=2048 # 512-byte sectors per MiB
# [ foreign ESP | "Windows" | target ESP | target boot | target root | staging ESP | staging root | recovery ]
sfdisk --wipe always "$loop" >/dev/null <<EOF
label: gpt
label-id: $disk_guid
start=$((1 * mib)), size=$((100 * mib)), type=c12a7328-f81f-11d2-ba4b-00a0c93ec93b, uuid=00000000-0000-4000-8000-00000000000a
start=$((101 * mib)), size=$((1024 * mib)), type=ebd0a0a2-b9e5-4433-87c0-68b6b72699c7, uuid=00000000-0000-4000-8000-00000000000b
start=$((1126 * mib)), size=$((256 * mib)), type=c12a7328-f81f-11d2-ba4b-00a0c93ec93b, uuid=$target_esp_uuid
start=$((1382 * mib)), size=$((1024 * mib)), type=0fc63daf-8483-4772-8e79-3d69d8477de4, uuid=$target_boot_uuid
start=$((2406 * mib)), size=$((2048 * mib)), type=7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab, uuid=$target_root_uuid
start=$((4454 * mib)), size=$((256 * mib)), type=c12a7328-f81f-11d2-ba4b-00a0c93ec93b, uuid=$esp_uuid
start=$((4710 * mib)), size=$((2048 * mib)), type=d4c4daf6-f72c-43ee-b8e3-5ba32550311d, uuid=$root_uuid
start=$((6758 * mib)), size=$((100 * mib)), type=ebd0a0a2-b9e5-4433-87c0-68b6b72699c7, uuid=$recovery_uuid
EOF
partx -u "$loop"
sync_partition_nodes
[ -b "${loop}p8" ] || { echo 'FAIL  the kernel did not register the loop partitions' >&2; exit 1; }

map() {
  sfdisk --json "$loop" | python3 -c 'import json,sys; t=json.load(sys.stdin)["partitiontable"]; print("\n".join(sorted("{}:{}:{}".format(p["uuid"].lower(),p["start"],p["size"]) for p in t["partitions"])))'
}

# --- same-disk guard against real lsblk JSON --------------------------------
# The staging root cannot really be this machine's /sysroot; inject exactly
# that mount into real lsblk output and keep every other field as reported.
fixture="$tmp/fixture"
mkdir -p "$fixture/efivars" "$fixture/esp/EFI/tilo-staging"
disk_bytes="$(blockdev --getsize64 "$loop")"
cat > "$fixture/esp/EFI/tilo-staging/handoff.json" <<EOF
{"schema":1,"operation_id":"0f0e0d0c-0b0a-4908-8706-050403020100","host_os":"windows",
 "disk_guid":"$disk_guid","disk_size_bytes":$disk_bytes,
 "staging_esp":{"partuuid":"$esp_uuid","first_lba":$((4454 * mib)),"last_lba":$((4710 * mib - 1))},
 "staging_root":{"partuuid":"$root_uuid","first_lba":$((4710 * mib)),"last_lba":$((6758 * mib - 1))},
 "target_extent":{"first_lba":$((1126 * mib)),"last_lba":$((4454 * mib - 1))},
 "owned_boot_entry":"0104","original_boot_order":["0001"],"profile":{}}
EOF
cp "$fixture/esp/EFI/tilo-staging/handoff.json" "$fixture/handoff.json"
python3 - "$fixture/efivars" "$esp_uuid" <<'PY'
import sys
open(sys.argv[1] + '/LoaderDevicePartUUID-4a67b082-0a4c-41cf-b6c7-440b29bb8c4f', 'wb').write(
    b'\x06\x00\x00\x00' + (sys.argv[2].upper() + '\0').encode('utf-16-le'))
PY
# Remove the not-yet-created target partitions so the extent is unallocated again.
sfdisk --delete "$loop" 3 4 5 >/dev/null
partx -u "$loop"
sync_partition_nodes
# Find the staging root's node from the GPT itself: without a udev database
# (CI containers) lsblk reports no PARTUUID to match on.
root_node="$(sfdisk --json "$loop" | python3 -c 'import json,sys; u=sys.argv[1].lower()
print(*[p["node"] for p in json.load(sys.stdin)["partitiontable"]["partitions"] if p["uuid"].lower() == u])' "$root_uuid")"
[ -n "$root_node" ] || { echo 'FAIL  staging root partition not found in the GPT' >&2; exit 1; }
lsblk -J -b -p -o NAME,TYPE,START,SIZE,PARTUUID,PARTTYPE,PTTYPE,PTUUID,LOG-SEC,MOUNTPOINTS "$loop" \
  | python3 -c 'import json,sys; d=json.load(sys.stdin); n=sys.argv[1]
d["blockdevices"][0]["type"] = "disk"  # lsblk reports loop devices as "loop"
hits = [c for c in d["blockdevices"][0]["children"] if c.get("name") == n]
assert len(hits) == 1, "staging root node missing from lsblk"
hits[0]["mountpoints"] = ["/sysroot"]
json.dump(d, sys.stdout)' "$root_node" > "$fixture/lsblk.json"
check() {
  TILO_STAGING_EFIVARS="$fixture/efivars" TILO_STAGING_ESP_DIR="$fixture/esp" \
  TILO_STAGING_LSBLK_JSON="$fixture/lsblk.json" \
    "$handoff_helper" check-install --handoff "$fixture/handoff.json" --disk "$loop" \
      --mode "$1" --sector-size 512 \
      --free-start-bytes $((1126 * mib * 512)) --free-size-bytes $((3328 * mib * 512)) \
      --efi-start-bytes $((1126 * mib * 512)) --efi-size-bytes $((2048 * mib * 512)) \
      --boot-start-bytes $((3174 * mib * 512)) --boot-size-bytes $((1024 * mib * 512)) \
      --root-start-bytes $((4198 * mib * 512)) --root-size-bytes $((256 * mib * 512))
}
# The loop layout is scaled down, so the real guided minimum refuses it; that
# refusal must come from the size rule, i.e. only after every identity matched.
if check guided > "$tmp/check.out" 2>&1; then
  echo 'FAIL  undersized staged extent was accepted' >&2; exit 1
fi
grep -Fq 'smaller than the guided minimum' "$tmp/check.out" \
  || { echo "FAIL  real lsblk output did not pass the identity checks: $(cat "$tmp/check.out")" >&2; exit 1; }
if check erase > "$tmp/check.out" 2>&1 || ! grep -Fq 'accepts only guided alongside' "$tmp/check.out"; then
  echo 'FAIL  whole-disk erase was not refused on the staged disk' >&2; exit 1
fi
printf 'ok    staged same-disk guard parses real lsblk identities and refuses erase\n'

# Recreate the installed target partitions exactly where the installer puts them.
sfdisk --append "$loop" >/dev/null <<EOF
start=$((1126 * mib)), size=$((256 * mib)), type=c12a7328-f81f-11d2-ba4b-00a0c93ec93b, uuid=$target_esp_uuid
start=$((1382 * mib)), size=$((1024 * mib)), type=0fc63daf-8483-4772-8e79-3d69d8477de4, uuid=$target_boot_uuid
start=$((2406 * mib)), size=$((2048 * mib)), type=7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab, uuid=$target_root_uuid
EOF
partx -u "$loop"
sync_partition_nodes
target_root="$(sfdisk --json "$loop" | python3 -c 'import json,sys; u=sys.argv[1]; print(next(p["node"] for p in json.load(sys.stdin)["partitiontable"]["partitions"] if p["uuid"].lower()==u))' "$target_root_uuid")"
[ -b "$target_root" ] || { echo "FAIL  the Tilo root partition $target_root has no node" >&2; exit 1; }
mkfs.btrfs -q -f "$target_root"
mkdir -p "$mnt"
mount "$target_root" "$mnt"
before_fs="$(btrfs filesystem show --raw "$mnt" | awk '/devid/ {print $4}')"

# --- reclaim ------------------------------------------------------------------
record="$tmp/var/lib/tilo/staging-reclaim.json"
mkdir -p "$(dirname "$record")" "$tmp/bin"
cat > "$record" <<EOF
{"schema":1,"operation_id":"0f0e0d0c-0b0a-4908-8706-050403020100","disk_guid":"$disk_guid",
 "disk_size_bytes":$disk_bytes,"sector_bytes":512,
 "staging_esp":{"partuuid":"$esp_uuid","first_lba":$((4454 * mib)),"last_lba":$((4710 * mib - 1))},
 "staging_root":{"partuuid":"$root_uuid","first_lba":$((4710 * mib)),"last_lba":$((6758 * mib - 1))},
 "target_root":{"partuuid":"$target_root_uuid","first_lba":$((2406 * mib)),"last_lba":$((4454 * mib - 1))},
 "target_esp_partuuid":"$target_esp_uuid"}
EOF
# Only the session gate is simulated; every disk tool is the real one.
cat > "$tmp/bin/loginctl" <<'SH'
#!/bin/sh
case "$1" in
  list-sessions) echo '2 1000 tilo seat0' ;;
  show-session) printf 'Class=user\nUid=1000\nState=active\n' ;;
esac
SH
chmod 0755 "$tmp/bin/loginctl"
run_reclaim() {
  PATH="$tmp/bin:$PATH" TILO_STAGING_RECLAIM_RECORD="$record" TILO_STAGING_RECLAIM_ROOT_MOUNT="$mnt" \
    TILO_STAGING_RECLAIM_ALLOW_LOOP=1 \
    "$reclaim"
}

# A single changed identity must leave the whole map untouched.
cp "$record" "$tmp/record.good"
sed -i "s/$root_uuid/ffffffff-0000-4000-8000-00000000000f/" "$record"
before_map="$(map)"
if run_reclaim > "$tmp/reclaim.out" 2>&1; then
  echo 'FAIL  mismatched reclaim record was accepted' >&2; exit 1
fi
[ "$(map)" = "$before_map" ] || { echo 'FAIL  refused reclaim changed the partition map' >&2; exit 1; }
[ -f "$record" ] || { echo 'FAIL  refused reclaim removed its evidence' >&2; exit 1; }
cp "$tmp/record.good" "$record"
printf 'ok    mismatched staging identity leaves the real GPT untouched\n'

run_reclaim || { echo 'FAIL  reclaim failed' >&2; exit 1; }
after_map="$(map)"
grep -q "^$esp_uuid:" <<<"$after_map" && { echo 'FAIL  staging ESP survived' >&2; exit 1; }
grep -q "^$root_uuid:" <<<"$after_map" && { echo 'FAIL  staging root survived' >&2; exit 1; }
grep -Fxq "$target_root_uuid:$((2406 * mib)):$((4352 * mib))" <<<"$after_map" \
  || { printf 'FAIL  Tilo root did not grow to the end of the staging space:\n%s\n' "$after_map" >&2; exit 1; }
grep -Fxq "$recovery_uuid:$((6758 * mib)):$((100 * mib))" <<<"$after_map" \
  || { echo 'FAIL  the partition after staging changed' >&2; exit 1; }
grep -Fxq "00000000-0000-4000-8000-00000000000b:$((101 * mib)):$((1024 * mib))" <<<"$after_map" \
  || { echo 'FAIL  the retained OS partition changed' >&2; exit 1; }
[ ! -e "$record" ] || { echo 'FAIL  reclaim record was not removed' >&2; exit 1; }
after_fs="$(btrfs filesystem show --raw "$mnt" | awk '/devid/ {print $4}')"
[ "$after_fs" -gt "$before_fs" ] || { echo 'FAIL  btrfs did not grow into the reclaimed space' >&2; exit 1; }
printf 'ok    reclaim deletes staging, grows the adjacent Tilo root and btrfs, and keeps every other partition\n'
