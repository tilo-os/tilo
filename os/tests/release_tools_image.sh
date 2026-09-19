#!/usr/bin/env bash
set -euo pipefail

image=${1:?usage: release_tools_image.sh IMAGE}

docker run --rm --entrypoint bash "$image" -euo pipefail -c '
  for compiler in cargo rustc cc gcc clang; do
    ! command -v "$compiler" || { echo "unexpected compiler: $compiler" >&2; exit 1; }
  done
  for tool in awk bash cmp curl find git grep gzip jq node python3 sed sha256sum tar; do
    command -v "$tool" >/dev/null || { echo "missing release tool: $tool" >&2; exit 1; }
  done
'
