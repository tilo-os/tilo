#!/usr/bin/env bash
set -euo pipefail

image=${1:?usage: release_tools_image.sh IMAGE}
max_image_bytes=536870912

image_bytes=$(docker image inspect --format '{{.Size}}' "$image")
[[ $image_bytes =~ ^[0-9]+$ ]]
(( image_bytes <= max_image_bytes )) || {
  printf 'release-tools image is unexpectedly large: %s bytes\n' "$image_bytes" >&2
  exit 1
}
printf 'release-tools image size: %s bytes\n' "$image_bytes"

docker run --rm --entrypoint bash "$image" -euo pipefail -c '
  for compiler in cargo rustc cc gcc clang; do
    ! command -v "$compiler" || { echo "unexpected compiler: $compiler" >&2; exit 1; }
  done
  for tool in awk bash cmp curl find git grep gzip jq node python3 sed sha256sum tar; do
    command -v "$tool" >/dev/null || { echo "missing release tool: $tool" >&2; exit 1; }
  done
'
