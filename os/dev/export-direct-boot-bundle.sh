#!/usr/bin/env bash
# Export a dev VM's direct-boot companion from the UKI that bootc installed.
# The caller supplies the ESP's GPT partition UUID from the same disk.
set -euo pipefail

if (( $# != 3 )); then
  echo 'usage: export-direct-boot-bundle.sh UKI.efi ESP-PARTUUID OUTPUT-DIR' >&2
  exit 2
fi
uki=$1 partuuid=$2 out=$3
[[ -f $uki && ! -L $uki ]] || { echo "not a regular UKI: $uki" >&2; exit 1; }
command -v objcopy >/dev/null
command -v python3 >/dev/null
[[ ! -e $out ]] || { echo "direct boot output already exists: $out" >&2; exit 1; }
mkdir -p "$(dirname "$out")"
tmp=$(mktemp -d "${out}.tmp.XXXXXX")
trap 'rm -rf -- "$tmp"' EXIT

# Keep the input UKI untouched. objcopy requires an output file even when all
# three sections are dumped separately; the disposable PE output is removed.
objcopy \
  --dump-section ".linux=$tmp/Image" \
  --dump-section ".initrd=$tmp/initrd" \
  --dump-section ".cmdline=$tmp/.cmdline" \
  "$uki" "$tmp/.discard.efi"
rm -f -- "$tmp/.discard.efi"
for file in Image initrd .cmdline; do
  [[ -s $tmp/$file ]] || { echo "UKI has no $file section" >&2; exit 1; }
done

python3 - "$tmp/.cmdline" "$partuuid" "$tmp/boot.json" <<'PY'
import json
import pathlib
import sys
import uuid

cmdline = pathlib.Path(sys.argv[1]).read_bytes().rstrip(b"\0").decode("utf-8")
if "\0" in cmdline or "root=tilo-auto" not in cmdline.split():
    raise SystemExit("UKI command line does not name the Tilo root resolver")
if "systemd.setenv=TILO_ROOT_LOADER_PARTUUID=" in cmdline:
    raise SystemExit("UKI command line already overrides the loader UUID")
partuuid = str(uuid.UUID(sys.argv[2]))
with open(sys.argv[3], "w", encoding="utf-8") as output:
    json.dump({"kernel": "Image", "initrd": "initrd", "cmdline": cmdline,
               "loader_partuuid": partuuid}, output, sort_keys=True)
    output.write("\n")
PY
rm -f -- "$tmp/.cmdline"
mv -- "$tmp" "$out"
trap - EXIT
