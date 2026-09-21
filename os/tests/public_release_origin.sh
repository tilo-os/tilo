#!/usr/bin/env bash
# Prove catalogs accept only the configured public binary origin.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
work="$(mktemp -d /var/tmp/tilo-public-origin-test.XXXXXX)"
trap 'find "$work" -depth -delete' EXIT
mkdir -p "$work/fragments"

write_fragment() {
  local arch="$1" url="$2"
  jq -n --arg arch "$arch" --arg url "$url" \
    '{name:"Tilo",version:"1.2.3",channel:"stable",arch:$arch,url:$url,
      archive_sha256:("a" * 64),archive_size:123,
      image_sha256:("b" * 64),image_size:456}' \
    >"$work/fragments/catalog-$arch.json"
}

prefix='https://downloads.example/tilo/public-downloads/releases/download/imager-v1.2.3'
write_fragment x86_64 "$prefix/tilo-installer-linux-x86_64.img.zst"
write_fragment aarch64 "$prefix/tilo-installer-linux-aarch64.img.zst"
GITHUB_REF_NAME=imager-v1.2.3 \
GITHUB_SERVER_URL=https://downloads.example \
TILO_RELEASE_REPOSITORY=tilo/public-downloads \
  "$repo/apps/imager/packaging/build-release-catalog.sh" \
    "$work/fragments" "$work/catalog"
jq -e --arg prefix "$prefix/" \
  'all(.releases[]; .url | startswith($prefix))' \
  "$work/catalog/tilo-installer-catalog-v1.json" >/dev/null

write_fragment x86_64 \
  'https://code.podesta.ai/qos/qos/releases/download/imager-v1.2.3/tilo-installer-linux-x86_64.img.zst'
if GITHUB_REF_NAME=imager-v1.2.3 \
   GITHUB_SERVER_URL=https://downloads.example \
   TILO_RELEASE_REPOSITORY=tilo/public-downloads \
     "$repo/apps/imager/packaging/build-release-catalog.sh" \
       "$work/fragments" "$work/rejected" >/dev/null 2>&1; then
  printf 'catalog accepted a private-source download URL\n' >&2
  exit 1
fi

printf 'ok    installer catalog is bound to the configured public release origin\n'
