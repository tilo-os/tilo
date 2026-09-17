#!/usr/bin/env bash
# Sign every catalogued medium for one native lane, after checking its bytes.
set -euo pipefail
catalog_root="${1:?catalog directory required}"
media_root="${2:?media directory required}"
arch="${3:?native architecture required}"
output="${4:?signature output directory required}"
packaging="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
catalog="$(python3 "$packaging/release-catalog.py" "$catalog_root")"
media_list="$(python3 "$packaging/release-catalog.py" "$catalog_root" \
  --media-root "$media_root" --arch "$arch")"
mkdir -p "$output"
while IFS= read -r medium; do
  bash "$packaging/sign-linux-release-file.sh" \
    "$medium" "$output/$(basename "$medium").asc"
done <<<"$media_list"
if [ "$arch" = x86_64 ]; then
  bash "$packaging/sign-linux-release-file.sh" \
    "$catalog" "$output/$(basename "$catalog").asc"
fi
