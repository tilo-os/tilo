#!/usr/bin/env bash
# Export a dev VM's direct-boot companion from what bootc installed on the ESP.
# The caller supplies the ESP's GPT partition UUID from the same disk.
#
#   export-direct-boot-bundle.sh UKI.efi ESP-PARTUUID OUTPUT-DIR
#       a sealed image: the kernel, initrd and command line of its UKI
#   export-direct-boot-bundle.sh --bls ENTRY.conf ESP-ROOT ESP-PARTUUID OUTPUT-DIR
#       an unsealed image (the dev image): the kernel, initrd and options of
#       its one Boot Loader Specification entry, paths relative to ESP-ROOT
set -euo pipefail

usage() {
  echo 'usage: export-direct-boot-bundle.sh UKI.efi ESP-PARTUUID OUTPUT-DIR' >&2
  echo '       export-direct-boot-bundle.sh --bls ENTRY.conf ESP-ROOT ESP-PARTUUID OUTPUT-DIR' >&2
  exit 2
}
mode=uki
if [[ ${1:-} == --bls ]]; then
  mode=bls
  shift
  (( $# == 4 )) || usage
  entry=$1 esp=$2 partuuid=$3 out=$4
  [[ -f $entry && ! -L $entry ]] || { echo "not a regular BLS entry: $entry" >&2; exit 1; }
  [[ -d $esp ]] || { echo "not an ESP directory: $esp" >&2; exit 1; }
else
  (( $# == 3 )) || usage
  uki=$1 partuuid=$2 out=$3
  [[ -f $uki && ! -L $uki ]] || { echo "not a regular UKI: $uki" >&2; exit 1; }
  command -v objcopy >/dev/null
fi
command -v python3 >/dev/null
[[ ! -e $out ]] || { echo "direct boot output already exists: $out" >&2; exit 1; }
mkdir -p "$(dirname "$out")"
tmp=$(mktemp -d "${out}.tmp.XXXXXX")
trap 'rm -rf -- "$tmp"' EXIT

if [[ $mode == uki ]]; then
  # Keep the input UKI untouched. objcopy requires an output file even when all
  # three sections are dumped separately; the disposable PE output is removed.
  objcopy \
    --dump-section ".linux=$tmp/Image" \
    --dump-section ".initrd=$tmp/initrd" \
    --dump-section ".cmdline=$tmp/.cmdline" \
    "$uki" "$tmp/.discard.efi"
  rm -f -- "$tmp/.discard.efi"
else
  # One kernel, one initrd and the options line; each path must stay inside
  # the ESP. The options carry the image's kargs.d, root=tilo-auto included.
  python3 - "$entry" "$esp" "$tmp" <<'PY'
import pathlib
import shutil
import sys

entry, esp, tmp = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2]).resolve(), pathlib.Path(sys.argv[3])
fields: dict[str, list[str]] = {}
for line in entry.read_text(encoding="utf-8").splitlines():
    line = line.strip()
    if not line or line.startswith("#"):
        continue
    key, _, value = line.partition(" ")
    fields.setdefault(key, []).append(value.strip())
for key in ("linux", "initrd", "options"):
    if len(fields.get(key, [])) != 1:
        raise SystemExit(f"BLS entry {entry} needs exactly one {key} line")
def inside(value: str) -> pathlib.Path:
    path = (esp / value.lstrip("/")).resolve()
    if esp not in path.parents or not path.is_file():
        raise SystemExit(f"BLS entry path {value} is not a file on the ESP")
    return path
shutil.copyfile(inside(fields["linux"][0]), tmp / "Image")
shutil.copyfile(inside(fields["initrd"][0]), tmp / "initrd")
(tmp / ".cmdline").write_text(fields["options"][0], encoding="utf-8")
PY
fi
for file in Image initrd .cmdline; do
  [[ -s $tmp/$file ]] || { echo "boot entry has no $file" >&2; exit 1; }
done

python3 - "$tmp/.cmdline" "$partuuid" "$tmp/boot.json" "$mode" <<'PY'
import json
import pathlib
import re
import sys
import uuid

cmdline = pathlib.Path(sys.argv[1]).read_bytes().rstrip(b"\0").decode("utf-8").strip()
if "\0" in cmdline or "root=tilo-auto" not in cmdline.split():
    raise SystemExit("boot command line does not name the Tilo root resolver")
if "systemd.setenv=TILO_ROOT_LOADER_PARTUUID=" in cmdline:
    raise SystemExit("boot command line already overrides the loader UUID")
# An unsealed dev BLS entry uses bootc's read-only ESP mount. Direct boot is
# only used with a throwaway VM overlay, where tilo-boot-entries must rename
# loader entries and maintain its repair metadata on /boot during first boot.
# Keep sealed UKI command lines byte-for-byte unchanged.
if sys.argv[4] == "bls":
    words = cmdline.split()
    for index, word in enumerate(words):
        match = re.fullmatch(r"systemd\.mount-extra=([^:]+):/boot:([^:]+):([^:]+)", word)
        if not match:
            continue
        options = match.group(3).split(",")
        if "ro" in options:
            options = ["rw" if option == "ro" else option for option in options]
            words[index] = f"systemd.mount-extra={match.group(1)}:/boot:{match.group(2)}:{','.join(options)}"
    cmdline = " ".join(words)
partuuid = str(uuid.UUID(sys.argv[2]))
with open(sys.argv[3], "w", encoding="utf-8") as output:
    json.dump({"kernel": "Image", "initrd": "initrd", "cmdline": cmdline,
               "loader_partuuid": partuuid}, output, sort_keys=True)
    output.write("\n")
PY
rm -f -- "$tmp/.cmdline"
mv -- "$tmp" "$out"
trap - EXIT
