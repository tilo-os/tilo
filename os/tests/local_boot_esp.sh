#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
script="$root/os/dev/make-boot-esp.sh"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

for command in sfdisk mformat mcopy mmd; do
  command -v "$command" >/dev/null || { echo "SKIP: missing $command"; exit 0; }
done

root_uuid=11111111-2222-4333-8444-555555555555
truncate -s 32M "$work/base.raw"
cat <<EOF | sfdisk "$work/base.raw" >/dev/null
label: gpt

size=16M, type=L, uuid=$root_uuid
EOF
printf 'candidate uki fixture\n' >"$work/tilo.efi"
base_before=$(sha256sum "$work/base.raw")

"$script" --base "$work/base.raw" --uki "$work/tilo.efi" \
  --root-partuuid "$root_uuid" --output "$work/esp.raw" --size-mib 64

test "$base_before" = "$(sha256sum "$work/base.raw")"
python3 - "$work/esp.raw.receipt.json" "$root_uuid" <<'PY'
import json, sys
d = json.load(open(sys.argv[1], encoding="utf-8"))
assert d["mode"] == "direct-uki"
assert d["base"]["root_partuuid"] == sys.argv[2]
assert len(d["esp"]["sha256"]) == 64
assert "BOOTAA64 EFI" in d["esp"]["directory_listing"]
PY

if "$script" --base "$work/base.raw" --uki "$work/tilo.efi" \
  --root-partuuid aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee \
  --output "$work/bad.raw" --size-mib 64 2>"$work/error"; then
  echo 'expected mismatched PARTUUID to fail' >&2
  exit 1
fi
grep -q 'expected root PARTUUID' "$work/error"

printf 'systemd-boot fixture\n' >"$work/systemd-bootaa64.efi"
"$script" --base "$work/base.raw" --uki "$work/tilo.efi" \
  --root-partuuid "$root_uuid" --output "$work/loader-esp.raw" --size-mib 64 \
  --bootloader "$work/systemd-bootaa64.efi"
python3 - "$work/loader-esp.raw.receipt.json" <<'PY'
import json, sys
d = json.load(open(sys.argv[1], encoding="utf-8"))
assert d["mode"] == "systemd-boot"
assert d["bootloader"]["sha256"]
listing = d["esp"]["directory_listing"]
assert "BOOTAA64 EFI" in listing
assert "TILO-L~1 EFI" in listing or "tilo-local efi" in listing.lower()
PY
echo 'local boot ESP tests passed'
