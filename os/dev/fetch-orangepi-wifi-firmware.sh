#!/usr/bin/env bash
# Stage the AP6256 firmware the Orange Pi 3B v2.1 Wi-Fi radio loads into
# os/hardware/firmware/orangepi-3b-v2-1/, with the manifest
# prepare-hardware-profile.py checks.
#
# Fedora's linux-firmware does not carry these files. The bytes are Armbian's
# published copies, pinned by SHA-256 in os/external-assets/github-assets.tsv
# and fetched through the shared asset cache. os/hardware/firmware is a local
# build input and stays out of the repository.
#
# The board's SDIO node has no brcm,board-type, so brcmfmac asks first for
# files named after the machine's first compatible string,
# xunlong,orangepi-3b-v2.1, and then for the generic names. Both are staged.
set -euo pipefail

repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
profile=orangepi-3b-v2-1
board=xunlong,orangepi-3b-v2.1
out="$repo/os/hardware/firmware/$profile"
cache="${TILO_GITHUB_ASSET_CACHE:-$repo/os/external-assets/cache}"

# Generic installed name -> asset name in github-assets.tsv.
declare -A assets=(
  [brcmfmac43456-sdio.bin]=brcmfmac43456-sdio-30d86fc.bin
  [brcmfmac43456-sdio.clm_blob]=brcmfmac43456-sdio-30d86fc.clm_blob
  [brcmfmac43456-sdio.txt]=brcmfmac43456-sdio-67ccb55.txt
)

TILO_GITHUB_ASSET_CACHE="$cache" "$repo/os/dev/cache-github-assets.sh" "${assets[@]}"

mkdir -p "$out"
for name in "${!assets[@]}"; do
  install -m 0644 "$cache/${assets[$name]}" "$out/$name"
  install -m 0644 "$cache/${assets[$name]}" "$out/${name/brcmfmac43456-sdio./brcmfmac43456-sdio.$board.}"
done

python3 - "$out" "$profile" <<'PY'
import hashlib, json, pathlib, sys
out, profile = pathlib.Path(sys.argv[1]), sys.argv[2]
files = {
    path.name: hashlib.sha256(path.read_bytes()).hexdigest()
    for path in sorted(out.iterdir())
    if path.name.startswith("brcmfmac43456-sdio.")
}
manifest = {"schema_version": 1, "profile": profile, "files": files}
(out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
PY
printf '[tilo] staged %s Wi-Fi firmware in %s\n' "$profile" "$out"
