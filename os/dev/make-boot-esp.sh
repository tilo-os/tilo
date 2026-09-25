#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: make-boot-esp.sh --base DISK --uki FILE --root-partuuid UUID --output ESP [--bootloader FILE]

Create a small GPT disk containing only an EFI System Partition.
The base OS disk is opened read-only and its expected root PARTUUID must exist.

Without --bootloader, the candidate UKI is installed as EFI/BOOT/BOOTAA64.EFI.
With --bootloader, that file becomes the fallback systemd-boot binary and the
UKI is installed as EFI/Linux/tilo-local.efi.
EOF
}

die() { printf 'error: %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null || die "required command is missing: $1"; }

base=
uki=
root_partuuid=
output=
bootloader=
size_mib=512

while (($#)); do
  case "$1" in
    --base) base=${2:?}; shift 2 ;;
    --uki) uki=${2:?}; shift 2 ;;
    --root-partuuid) root_partuuid=${2:?}; shift 2 ;;
    --output) output=${2:?}; shift 2 ;;
    --bootloader) bootloader=${2:?}; shift 2 ;;
    --size-mib) size_mib=${2:?}; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done

[[ -r "$base" && -f "$base" ]] || die "base disk is not a readable regular file: $base"
[[ -r "$uki" && -f "$uki" ]] || die "candidate UKI is not readable: $uki"
[[ -z "$bootloader" || (-r "$bootloader" && -f "$bootloader") ]] || die "bootloader is not readable: $bootloader"
[[ "$root_partuuid" =~ ^[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}$ ]] \
  || die "--root-partuuid must be a UUID"
[[ "$size_mib" =~ ^[0-9]+$ && "$size_mib" -ge 64 ]] || die "--size-mib must be an integer of at least 64"
[[ ! -e "$output" ]] || die "refusing to replace existing output: $output"

for cmd in python3 sha256sum sfdisk truncate mformat mmd mcopy mdir stat; do need "$cmd"; done

output=$(python3 -c 'import os,sys; print(os.path.abspath(sys.argv[1]))' "$output")
receipt="$output.receipt.json"
mkdir -p "$(dirname "$output")"
work=$(mktemp -d "${TMPDIR:-/tmp}/tilo-esp.XXXXXX")
cleanup() { rm -rf "$work"; }
trap cleanup EXIT

# Capture the partition identities and file metadata before doing any work. The
# script never passes the base disk to a command that opens it for writing.
sfdisk --json "$base" >"$work/base-before.json"
base_size_before=$(stat -c %s "$base")
base_mtime_before=$(stat -c %Y "$base")

python3 - "$work/base-before.json" "$root_partuuid" <<'PY'
import json, sys
doc = json.load(open(sys.argv[1], encoding="utf-8"))
wanted = sys.argv[2].lower()
matches = [p for p in doc["partitiontable"].get("partitions", [])
           if p.get("uuid", "").lower() == wanted]
if len(matches) != 1:
    raise SystemExit(f"expected root PARTUUID {wanted} exactly once; found {len(matches)}")
PY

uki_sha=$(sha256sum "$uki" | cut -d' ' -f1)
ids=$(python3 - "$uki_sha" <<'PY'
import hashlib, sys, uuid
seed = bytes.fromhex(sys.argv[1])
def derived(label):
    raw = bytearray(hashlib.sha256(seed + label).digest()[:16])
    raw[6] = (raw[6] & 0x0f) | 0x40
    raw[8] = (raw[8] & 0x3f) | 0x80
    return str(uuid.UUID(bytes=bytes(raw)))
print(derived(b"tilo-local-esp-disk"), derived(b"tilo-local-esp-partition"))
PY
)
read -r disk_uuid esp_uuid <<<"$ids"
fat_serial=${uki_sha:0:8}

truncate -s "$((size_mib * 1024 * 1024))" "$output"
cat >"$work/layout.sfdisk" <<EOF
label: gpt
label-id: $disk_uuid
unit: sectors
first-lba: 2048

start=2048, type=U, uuid=$esp_uuid, name="Tilo local boot override"
EOF
sfdisk "$output" <"$work/layout.sfdisk" >/dev/null
sfdisk --json "$output" >"$work/esp-table.json"

read -r esp_start esp_size < <(python3 - "$work/esp-table.json" <<'PY'
import json, sys
p = json.load(open(sys.argv[1], encoding="utf-8"))["partitiontable"]["partitions"]
if len(p) != 1:
    raise SystemExit("override disk must contain exactly one partition")
print(p[0]["start"] * 512, p[0]["size"] * 512)
PY
)

mformat -i "$output@@$esp_start" -F -N "$fat_serial" -v TILOBOOT \
  -T "$((esp_size / 512))" ::
mmd -i "$output@@$esp_start" ::/EFI ::/EFI/BOOT

if [[ -n "$bootloader" ]]; then
  mmd -i "$output@@$esp_start" ::/EFI/Linux ::/loader
  mcopy -i "$output@@$esp_start" "$bootloader" ::/EFI/BOOT/BOOTAA64.EFI
  mcopy -i "$output@@$esp_start" "$uki" ::/EFI/Linux/tilo-local.efi
  printf 'default @saved\ntimeout 1\nconsole-mode keep\n' >"$work/loader.conf"
  mcopy -i "$output@@$esp_start" "$work/loader.conf" ::/loader/loader.conf
  mode=systemd-boot
else
  mcopy -i "$output@@$esp_start" "$uki" ::/EFI/BOOT/BOOTAA64.EFI
  mode=direct-uki
fi

mdir -i "$output@@$esp_start" -/ :: >"$work/directory.txt"
sfdisk --json "$base" >"$work/base-after.json"
base_size_after=$(stat -c %s "$base")
base_mtime_after=$(stat -c %Y "$base")
base_table_before=$(sha256sum "$work/base-before.json" | cut -d' ' -f1)
base_table_after=$(sha256sum "$work/base-after.json" | cut -d' ' -f1)
[[ "$base_table_before" == "$base_table_after" ]] \
  || die "base disk partition identities changed while creating the override"
[[ "$base_size_before:$base_mtime_before" == "$base_size_after:$base_mtime_after" ]] \
  || die "base disk metadata changed while creating the override"

esp_sha=$(sha256sum "$output" | cut -d' ' -f1)
bootloader_sha=
[[ -z "$bootloader" ]] || bootloader_sha=$(sha256sum "$bootloader" | cut -d' ' -f1)

python3 - "$receipt" "$base" "$work/base-after.json" "$root_partuuid" \
  "$base_size_after" "$base_mtime_after" "$uki" "$uki_sha" "$bootloader" \
  "$bootloader_sha" "$output" "$esp_sha" "$mode" "$disk_uuid" "$esp_uuid" \
  "$fat_serial" "$work/directory.txt" <<'PY'
import hashlib, json, os, sys
(receipt, base, table_path, root_uuid, base_size, base_mtime, uki, uki_sha,
 bootloader, bootloader_sha, output, esp_sha, mode, disk_uuid, esp_uuid,
 fat_serial, directory_path) = sys.argv[1:]
table = json.load(open(table_path, encoding="utf-8"))
canonical = json.dumps(table, sort_keys=True, separators=(",", ":")).encode()
doc = {
  "schema": 1,
  "mode": mode,
  "base": {
    "path": os.path.abspath(base),
    "size": int(base_size),
    "mtime_epoch": int(base_mtime),
    "partition_table_sha256": hashlib.sha256(canonical).hexdigest(),
    "root_partuuid": root_uuid.lower(),
    "partition_table": table,
  },
  "uki": {"path": os.path.abspath(uki), "sha256": uki_sha},
  "bootloader": None if not bootloader else {
    "path": os.path.abspath(bootloader), "sha256": bootloader_sha},
  "esp": {
    "path": output, "sha256": esp_sha, "disk_uuid": disk_uuid,
    "partition_uuid": esp_uuid, "fat_serial": fat_serial,
    "directory_listing": open(directory_path, encoding="utf-8", errors="replace").read(),
  },
}
with open(receipt, "w", encoding="utf-8") as f:
    json.dump(doc, f, indent=2, sort_keys=True)
    f.write("\n")
PY

printf 'Created %s (%s)\nReceipt: %s\n' "$output" "$mode" "$receipt"
