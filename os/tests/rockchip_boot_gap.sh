#!/bin/bash
# The Orange Pi disk refuses a loader gap the mask ROM cannot use, before it
# inspects or writes an image.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
disk="$repo/os/dev/disk-image.sh"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

if "$disk" "$work/image" "$work/out" --boot-gap 8M >"$work/small.log" 2>&1; then
  printf 'FAIL  an 8M boot gap was accepted\n' >&2
  exit 1
fi
grep -Fq 'at least 16M' "$work/small.log"

mkdir -p "$work/loader"
printf 'idb\n' >"$work/loader/idbloader.img"
if "$disk" "$work/image" "$work/out" --boot-gap 16M --rockchip-loader "$work/loader" \
    >"$work/missing.log" 2>&1; then
  printf 'FAIL  a loader directory without u-boot.itb was accepted\n' >&2
  exit 1
fi
grep -Fq 'missing u-boot.itb' "$work/missing.log"

printf 'ok    Rockchip boot gap is refused before any disk write\n'
