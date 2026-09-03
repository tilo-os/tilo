#!/usr/bin/env bash
# Destructive loopback proof for guided alongside partition creation. The test
# snapshots both the existing GPT entry and bytes inside it, then requires the
# helper to append exactly two partitions without changing either snapshot.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
helper="$repo_root/os/usr/libexec/qos-guided-partition"

if [ "$(id -u)" -ne 0 ] && ! sudo -n true >/dev/null 2>&1; then
  printf 'SKIP  guided alongside loopback test needs root or passwordless sudo\n'
  exit 0
fi
run_root() { if [ "$(id -u)" -eq 0 ]; then "$@"; else sudo -n "$@"; fi; }

tmp="$(mktemp -d)"
loop=""
cleanup() {
  [ -z "$loop" ] || run_root losetup -d "$loop" 2>/dev/null || true
  rm -rf "$tmp"
}
trap cleanup EXIT

truncate -s 40G "$tmp/disk.img"
loop="$(run_root losetup --find --show --partscan "$tmp/disk.img")"
existing_uuid=00000000-0000-4000-8000-000000000001
efi_uuid=10000000-0000-4000-8000-000000000002
root_uuid=20000000-0000-4000-8000-000000000003
printf 'label: gpt\nstart=2048, size=8192, type=0fc63daf-8483-4772-8e79-3d69d8477de4, uuid=%s, name="existing OS sentinel"\n' "$existing_uuid" \
  | run_root sfdisk --wipe always "$loop" >/dev/null
run_root blockdev --rereadpt "$loop"
run_root partx -u "$loop"
for _ in {1..50}; do [ -b "${loop}p1" ] && break; sleep 0.1; done
existing="$(run_root sfdisk --json "$loop" | python3 -c 'import json,sys; u=sys.argv[1]; print(next(p["node"] for p in json.load(sys.stdin)["partitiontable"]["partitions"] if p["uuid"].lower()==u))' "$existing_uuid")"
[ -b "$existing" ]
printf 'existing-operating-system-data\n' | run_root dd of="$existing" bs=1 seek=4096 conv=notrunc status=none
before_hash="$(run_root dd if="$existing" bs=1 skip=4096 count=31 status=none | sha256sum)"
before_entry="$(run_root sfdisk --json "$loop" | python3 -c 'import json,sys; t=json.load(sys.stdin)["partitiontable"]; p=t["partitions"][0]; print("{}:{}:{}".format(p["uuid"].lower(),p["start"]*t["sectorsize"],p["size"]*t["sectorsize"]))')"

disk_size=$((40 * 1024 * 1024 * 1024))
free_start=$((5 * 1024 * 1024))
free_size=$((disk_size - 6 * 1024 * 1024))
efi_size=$((2 * 1024 * 1024 * 1024))
root_start=$((free_start + efi_size))
root_size=$((free_size - efi_size))
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
    SERIAL) printf 'QOS-LOOPBACK-TEST\n'; exit 0 ;;
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
  --expect-disk-serial QOS-LOOPBACK-TEST --expect-disk-ptuuid "$ptuuid"
  --expect-logical-sector-size 512
  --free-start-bytes "$free_start" --free-size-bytes "$free_size"
  --efi-start-bytes "$free_start" --efi-size-bytes "$efi_size"
  --root-start-bytes "$root_start" --root-size-bytes "$root_size"
  --new-efi-partuuid "$efi_uuid" --new-root-partuuid "$root_uuid"
  --expect-partition "$existing_uuid:1048576:4194304"
)
if [ "$(id -u)" -eq 0 ]; then
  PATH="$tmp/bin:$PATH" "$helper" "${helper_args[@]}" >/dev/null
else
  sudo -n env "PATH=$tmp/bin:/usr/sbin:/usr/bin" "$helper" "${helper_args[@]}" >/dev/null
fi

after_hash="$(run_root dd if="$existing" bs=1 skip=4096 count=31 status=none | sha256sum)"
after_entry="$(run_root sfdisk --json "$loop" | python3 -c 'import json,sys; t=json.load(sys.stdin)["partitiontable"]; u=sys.argv[1]; p=next(p for p in t["partitions"] if p["uuid"].lower()==u); print("{}:{}:{}".format(p["uuid"].lower(),p["start"]*t["sectorsize"],p["size"]*t["sectorsize"]))' "$existing_uuid")"
[ "$after_hash" = "$before_hash" ] || { echo 'FAIL  existing partition bytes changed' >&2; exit 1; }
[ "$after_entry" = "$before_entry" ] || { echo 'FAIL  existing GPT entry changed' >&2; exit 1; }
[ "$(run_root sfdisk --json "$loop" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)["partitiontable"]["partitions"]))')" -eq 3 ] \
  || { echo 'FAIL  helper did not append exactly two partitions' >&2; exit 1; }
after_map="$(run_root sfdisk --json "$loop" | python3 -c 'import json,sys; t=json.load(sys.stdin)["partitiontable"]; s=t["sectorsize"]; print("\n".join(sorted("{}:{}:{}".format(p["uuid"].lower(),p["start"]*s,p["size"]*s) for p in t["partitions"])))')"
expected_map="$(printf '%s\n' \
  "$existing_uuid:1048576:4194304" \
  "$efi_uuid:$free_start:$efi_size" \
  "$root_uuid:$root_start:$root_size" | sort)"
[ "$after_map" = "$expected_map" ] \
  || { printf 'FAIL  final PARTUUID/geometry map differs from the reviewed plan\nexpected:\n%s\nactual:\n%s\n' "$expected_map" "$after_map" >&2; exit 1; }
types="$(run_root sfdisk --json "$loop" | python3 -c 'import json,sys; print("\n".join("{}={}".format(p["uuid"].lower(),p["type"].lower()) for p in json.load(sys.stdin)["partitiontable"]["partitions"]))')"
grep -Fqx "$efi_uuid=c12a7328-f81f-11d2-ba4b-00a0c93ec93b" <<<"$types"
grep -Fqx "$root_uuid=0fc63daf-8483-4772-8e79-3d69d8477de4" <<<"$types"
printf 'ok    guided alongside appends only its ESP and root; existing GPT entry and bytes are unchanged\n'
