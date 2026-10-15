#!/usr/bin/env bash
# The release-tools image (os/Containerfile.release-tools) before it is published: small, every
# publication tool present and no compiler, since the release jobs that run in it hold
# credentials. usage: release_tools_image.sh ARCHIVE (an OCI archive, as
# tools/ci/publish/oci-build.sh writes it). Runs in the buildah image.
set -euo pipefail

archive=${1:?usage: release_tools_image.sh ARCHIVE}
max_image_bytes=536870912
run=$(dirname -- "${BASH_SOURCE[0]}")/../../tools/ci/publish/oci-run.sh

bash "$run" "$archive" bash -euo pipefail -c '
  image_bytes=$(du -sxb / | cut -f1)
  (( image_bytes <= '"$max_image_bytes"' )) || {
    printf "release-tools image is unexpectedly large: %s bytes\n" "$image_bytes" >&2
    exit 1
  }
  printf "release-tools image size: %s bytes\n" "$image_bytes"
  for compiler in cargo rustc cc gcc clang; do
    ! command -v "$compiler" || { echo "unexpected compiler: $compiler" >&2; exit 1; }
  done
  for tool in awk bash cmp curl find git grep gzip jq node python3 sed sha256sum tar; do
    command -v "$tool" >/dev/null || { echo "missing release tool: $tool" >&2; exit 1; }
  done
'
