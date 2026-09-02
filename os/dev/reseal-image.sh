#!/usr/bin/env bash
# Add the deterministic, /boot-only composefs UKI reseal layer to an image.
set -euo pipefail

source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"

[ "$#" -eq 2 ] || die "usage: $0 IMAGE SHA512_HEX"
image="$1"
digest="$2"
[ "${#digest}" -eq 128 ] && case "$digest" in *[!0-9a-f]*) false ;; *) true ;; esac \
  || die "composefs digest must be exactly 128 lowercase hexadecimal characters"
podman image exists "$image" || die "image is not present in local storage: $image"

stage="$(mktemp -d "${TMPDIR:-/tmp}/qos-reseal.XXXXXX")"
trap 'rm -rf "$stage"' EXIT
cp "$QOS_OS_DIR/dev/uki-cmdline.py" "$stage/uki-cmdline.py"
build_args=(-t "$image" -f - --build-arg "BASE_IMAGE=$image")
if [ -n "${SOURCE_DATE_EPOCH:-}" ]; then
  case "$SOURCE_DATE_EPOCH" in *[!0-9]*|'') die "SOURCE_DATE_EPOCH must be an integer" ;; esac
  build_args+=(--timestamp "$SOURCE_DATE_EPOCH")
fi
podman build "${build_args[@]}" "$stage" <<EOF
ARG BASE_IMAGE
FROM \${BASE_IMAGE}
RUN --mount=type=bind,source=uki-cmdline.py,target=/run/uki-cmdline.py,z \
    python3 /run/uki-cmdline.py set "composefs=$digest" \
 && python3 /run/uki-cmdline.py show
EOF
