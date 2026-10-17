#!/usr/bin/env bash
# Destructive loopback proof for guided alongside partition creation. The test
# snapshots both the existing GPT entry and bytes inside it, then requires the
# helper to append exactly three partitions without changing either snapshot.
set -Eeuo pipefail
# Every unexpected non-zero exit names the command that failed; the invariant
# runner shows only this script's output.
trap 'printf "FAIL  guided alongside loopback test: line %s exited %s: %s\n" "$LINENO" "$?" "$BASH_COMMAND" >&2' ERR

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
helper="$repo_root/os/usr/libexec/tilo-guided-partition"

if [ "$(id -u)" -ne 0 ] && ! sudo -n true >/dev/null 2>&1; then
  printf 'SKIP  guided alongside loopback test needs root or passwordless sudo\n'
  exit 0
fi
run_root() { if [ "$(id -u)" -eq 0 ]; then "$@"; else sudo -n "$@"; fi; }

# The helper waits for the kernel's new partition nodes to appear in /dev. Only
# devtmpfs creates them; a container /dev on tmpfs is a static snapshot where
# they never appear, so the proof cannot run there.
dev_fstype="$(findmnt -no FSTYPE /dev 2>/dev/null || echo unknown)"
if [ "$dev_fstype" != devtmpfs ]; then
  printf 'SKIP  guided alongside loopback test needs a devtmpfs /dev that shows new partition nodes (/dev is %s)\n' \
    "$dev_fstype"
  exit 0
fi

# The helper hands its geometry and GPT JSON questions to the compiled
# installer helper; run this checkout's build of it.
cargo build --quiet --manifest-path "$repo_root/Cargo.toml" -p tilo-install-helpers --bin tilo-staging-handoff
staging_handoff="${CARGO_TARGET_DIR:-$repo_root/target}/debug/tilo-staging-handoff"

tmp="$(mktemp -d)"
loop=""
cleanup() {
  [ -z "$loop" ] || run_root losetup -d "$loop" 2>/dev/null || true
  rm -rf "$tmp"
}
trap cleanup EXIT

truncate -s 40G "$tmp/disk.img"
loop_error="$tmp/losetup.error"
if ! loop="$(run_root losetup --find --show --partscan "$tmp/disk.img" 2>"$loop_error")"; then
  printf 'SKIP  guided alongside loopback test needs a usable loop device (%s)\n' \
    "$(tr '\n' ' ' <"$loop_error")"
  exit 0
fi
existing_uuid=00000000-0000-4000-8000-000000000001
efi_uuid=10000000-0000-4000-8000-000000000002
boot_uuid=30000000-0000-4000-8000-000000000004
root_uuid=20000000-0000-4000-8000-000000000003
printf 'label: gpt\nstart=2048, size=8192, type=0fc63daf-8483-4772-8e79-3d69d8477de4, uuid=%s, name="existing OS sentinel"\n' "$existing_uuid" \
  | run_root sfdisk --wipe always "$loop" >/dev/null
run_root blockdev --rereadpt "$loop"
run_root partx -u "$loop"
for _ in {1..50}; do [ -b "${loop}p1" ] && break; sleep 0.1; done
if [ ! -b "${loop}p1" ]; then
  loop_name="${loop##*/}"
  printf 'FAIL  %sp1 never appeared on devtmpfs /dev (kernel partition: %s, loop partscan: %s)\n' \
    "$loop" \
    "$([ -e "/sys/class/block/${loop_name}p1" ] && echo registered || echo absent)" \
    "$(cat "/sys/block/$loop_name/loop/partscan" 2>/dev/null || echo unknown)" >&2
  exit 1
fi
existing="$(run_root sfdisk --json "$loop" | python3 -c 'import json,sys; u=sys.argv[1]; print(next(p["node"] for p in json.load(sys.stdin)["partitiontable"]["partitions"] if p["uuid"].lower()==u))' "$existing_uuid")"
[ -b "$existing" ] || { printf 'FAIL  existing partition node %s is not a block device\n' "$existing" >&2; exit 1; }
printf 'existing-operating-system-data\n' | run_root dd of="$existing" bs=1 seek=4096 conv=notrunc status=none
before_hash="$(run_root dd if="$existing" bs=1 skip=4096 count=31 status=none | sha256sum)"
before_entry="$(run_root sfdisk --json "$loop" | python3 -c 'import json,sys; t=json.load(sys.stdin)["partitiontable"]; p=t["partitions"][0]; print("{}:{}:{}".format(p["uuid"].lower(),p["start"]*t["sectorsize"],p["size"]*t["sectorsize"]))')"

disk_size=$((40 * 1024 * 1024 * 1024))
free_start=$((5 * 1024 * 1024))
free_size=$((disk_size - 6 * 1024 * 1024))
efi_size=$((2 * 1024 * 1024 * 1024))
boot_start=$((free_start + efi_size))
boot_size=$((1024 * 1024 * 1024))
root_start=$((boot_start + boot_size))
root_size=$((free_size - efi_size - boot_size))
ptuuid="$(run_root sfdisk --json "$loop" | python3 -c 'import json,sys; print(json.load(sys.stdin)["partitiontable"]["id"])')"
devnum="$(lsblk -dno MAJ:MIN "$loop" | xargs)"

# Loop devices do not expose a hardware serial. Wrap only that one lsblk query
# so the helper's production stable-identity gate remains exercised.
mkdir "$tmp/bin"
sed "s|@LOOP@|$loop|g" > "$tmp/bin/lsblk" <<'SH'
#!/bin/sh
if [ "$#" -eq 3 ] && [ "$1" = -dno ] && [ "$3" = @LOOP@ ]; then
  case "$2" in
    TYPE) printf 'disk\n'; exit 0 ;;
    SERIAL) printf 'TILO-LOOPBACK-TEST\n'; exit 0 ;;
    PTTYPE) printf 'gpt\n'; exit 0 ;;
    PTUUID) printf '@PTUUID@\n'; exit 0 ;;
  esac
  exec /usr/bin/lsblk "$@"
else
  exec /usr/bin/lsblk "$@"
fi
SH
sed -i "s|@PTUUID@|$ptuuid|g" "$tmp/bin/lsblk"
chmod 0755 "$tmp/bin/lsblk"

helper_args=(
  create --disk "$loop"
  --expect-disk-size-bytes "$disk_size" --expect-disk-devnum "$devnum"
  --expect-disk-serial TILO-LOOPBACK-TEST --expect-disk-ptuuid "$ptuuid"
  --expect-logical-sector-size 512
  --free-start-bytes "$free_start" --free-size-bytes "$free_size"
  --efi-start-bytes "$free_start" --efi-size-bytes "$efi_size"
  --boot-start-bytes "$boot_start" --boot-size-bytes "$boot_size"
  --root-start-bytes "$root_start" --root-size-bytes "$root_size"
  --new-efi-partuuid "$efi_uuid" --new-boot-partuuid "$boot_uuid" --new-root-partuuid "$root_uuid"
  --expect-partition "$existing_uuid:1048576:4194304"
)
invoke_helper() {
  if [ "$(id -u)" -eq 0 ]; then
    PATH="$tmp/bin:$PATH" TILO_STAGING_HANDOFF="$staging_handoff" "$helper" "${helper_args[@]}" >/dev/null
  else
    sudo -n env "PATH=$tmp/bin:/usr/sbin:/usr/bin" TILO_STAGING_HANDOFF="$staging_handoff" \
      "$helper" "${helper_args[@]}" >/dev/null
  fi
}

# An existing Tilo root anywhere on the disk refuses the plan before any write:
# the installed UKI resolves root=tilo-auto to exactly one Tilo root.
run_root sfdisk --part-type "$loop" 1 7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab >/dev/null 2>&1
if invoke_helper 2>"$tmp/second-root.error"; then
  echo 'FAIL  helper created a second Tilo root on the disk' >&2
  exit 1
fi
grep -q 'already holds a Tilo root' "$tmp/second-root.error" \
  || { cat "$tmp/second-root.error" >&2; echo 'FAIL  second Tilo root refused for the wrong reason' >&2; exit 1; }
[ "$(run_root sfdisk --json "$loop" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)["partitiontable"]["partitions"]))')" -eq 1 ] \
  || { echo 'FAIL  refused plan changed the partition map' >&2; exit 1; }
run_root sfdisk --part-type "$loop" 1 0fc63daf-8483-4772-8e79-3d69d8477de4 >/dev/null 2>&1

invoke_helper

after_hash="$(run_root dd if="$existing" bs=1 skip=4096 count=31 status=none | sha256sum)"
after_entry="$(run_root sfdisk --json "$loop" | python3 -c 'import json,sys; t=json.load(sys.stdin)["partitiontable"]; u=sys.argv[1]; p=next(p for p in t["partitions"] if p["uuid"].lower()==u); print("{}:{}:{}".format(p["uuid"].lower(),p["start"]*t["sectorsize"],p["size"]*t["sectorsize"]))' "$existing_uuid")"
[ "$after_hash" = "$before_hash" ] || { echo 'FAIL  existing partition bytes changed' >&2; exit 1; }
[ "$after_entry" = "$before_entry" ] || { echo 'FAIL  existing GPT entry changed' >&2; exit 1; }
[ "$(run_root sfdisk --json "$loop" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)["partitiontable"]["partitions"]))')" -eq 4 ] \
  || { echo 'FAIL  helper did not append exactly three partitions' >&2; exit 1; }
after_map="$(run_root sfdisk --json "$loop" | python3 -c 'import json,sys; t=json.load(sys.stdin)["partitiontable"]; s=t["sectorsize"]; print("\n".join(sorted("{}:{}:{}".format(p["uuid"].lower(),p["start"]*s,p["size"]*s) for p in t["partitions"])))')"
expected_map="$(printf '%s\n' \
  "$existing_uuid:1048576:4194304" \
  "$efi_uuid:$free_start:$efi_size" \
  "$boot_uuid:$boot_start:$boot_size" \
  "$root_uuid:$root_start:$root_size" | sort)"
[ "$after_map" = "$expected_map" ] \
  || { printf 'FAIL  final PARTUUID/geometry map differs from the reviewed plan\nexpected:\n%s\nactual:\n%s\n' "$expected_map" "$after_map" >&2; exit 1; }
types="$(run_root sfdisk --json "$loop" | python3 -c 'import json,sys; print("\n".join("{}={}".format(p["uuid"].lower(),p["type"].lower()) for p in json.load(sys.stdin)["partitiontable"]["partitions"]))')"
grep -Fqx "$efi_uuid=c12a7328-f81f-11d2-ba4b-00a0c93ec93b" <<<"$types"
grep -Fqx "$boot_uuid=0fc63daf-8483-4772-8e79-3d69d8477de4" <<<"$types"
grep -Fqx "$root_uuid=7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab" <<<"$types"

# A second invocation must refuse even the exact UUID/geometry pair. Those
# values live on the writable target and cannot authenticate Tilo ownership.
efi_partition="$(run_root sfdisk --json "$loop" | python3 -c 'import json,sys; u=sys.argv[1]; print(next(p["node"] for p in json.load(sys.stdin)["partitiontable"]["partitions"] if p["uuid"].lower()==u))' "$efi_uuid")"
root_partition="$(run_root sfdisk --json "$loop" | python3 -c 'import json,sys; u=sys.argv[1]; print(next(p["node"] for p in json.load(sys.stdin)["partitiontable"]["partitions"] if p["uuid"].lower()==u))' "$root_uuid")"
efi_fs_uuid="$(run_root blkid -p -s UUID -o value "$efi_partition")"
if invoke_helper; then
  echo 'FAIL  exact on-disk identities authorized reinitialization' >&2
  exit 1
fi
[ "$(run_root blkid -p -s UUID -o value "$efi_partition")" = "$efi_fs_uuid" ] \
  || { echo 'FAIL  refused retry changed the existing Tilo ESP' >&2; exit 1; }
[ "$(run_root sfdisk --json "$loop" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)["partitiontable"]["partitions"]))')" -eq 4 ] \
  || { echo 'FAIL  refused retry changed the partition map' >&2; exit 1; }

# Model power loss after the GPT commit but before mkfs. Even an unformatted
# pair with the exact reviewed UUIDs must remain ineligible: a target-local
# transaction record or identifiers could be forged by an arbitrary disk.
run_root wipefs -a "$efi_partition" >/dev/null
[ -z "$(run_root blkid -p -s TYPE -o value "$efi_partition" 2>/dev/null || true)" ]
if invoke_helper; then
  echo 'FAIL  unformatted on-disk pair authorized reinitialization' >&2
  exit 1
fi
[ -z "$(run_root blkid -p -s TYPE -o value "$efi_partition" 2>/dev/null || true)" ] \
  || { echo 'FAIL  refused interrupted pair was formatted' >&2; exit 1; }
[ -z "$(run_root blkid -p -s TYPE -o value "$root_partition" 2>/dev/null || true)" ] \
  || { echo 'FAIL  guided root acquired a filesystem signature' >&2; exit 1; }
[ "$(run_root dd if="$existing" bs=1 skip=4096 count=31 status=none | sha256sum)" = "$before_hash" ] \
  || { echo 'FAIL  refusal changed existing partition bytes' >&2; exit 1; }
[ "$(run_root sfdisk --json "$loop" | python3 -c 'import json,sys; t=json.load(sys.stdin)["partitiontable"]; s=t["sectorsize"]; print("\n".join(sorted("{}:{}:{}".format(p["uuid"].lower(),p["start"]*s,p["size"]*s) for p in t["partitions"])))')" = "$expected_map" ] \
  || { echo 'FAIL  refusal changed the reviewed PARTUUID/geometry map' >&2; exit 1; }

printf 'ok    guided alongside preserves existing bytes/map and refuses unauthenticated restart recovery\n'
