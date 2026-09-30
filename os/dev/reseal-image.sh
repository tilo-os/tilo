#!/usr/bin/env bash
# Add the deterministic, /boot-only composefs UKI reseal layer to an image.
set -euo pipefail

source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"

[ "$#" -ge 2 ] || die "usage: $0 IMAGE SHA512_HEX [--iidfile NEW_FILE --source-date-epoch SECONDS]"
image="$1"
digest="$2"
shift 2
iidfile=""; epoch=""
while [ "$#" -gt 0 ]; do
  [ "$#" -ge 2 ] || die "missing option value: $1"
  [ -n "$2" ] || die "empty option value: $1"
  case "$1" in
    --iidfile) [ -z "$iidfile" ] || die "duplicate --iidfile"; iidfile="$2" ;;
    --source-date-epoch) [ -z "$epoch" ] || die "duplicate --source-date-epoch"; epoch="$2" ;;
    *) die "unknown option: $1" ;;
  esac
  shift 2
done
[ "${#digest}" -eq 128 ] && case "$digest" in *[!0-9a-f]*) false ;; *) true ;; esac \
  || die "composefs digest must be exactly 128 lowercase hexadecimal characters"
podman_args=()
if [ -n "$iidfile" ]; then
  # This mode is an immutable intermediate, not publication or boot proof.
  [[ "$image" =~ ^sha256:[0-9a-f]{64}$ ]] || die "immutable reseal requires a full local image ID"
  [[ "$epoch" =~ ^(0|[1-9][0-9]{0,11})$ ]] && [ "$epoch" -le 253402300799 ] \
    || die "immutable reseal requires an explicit canonical source date epoch"
  [ ! -e "$iidfile" ] && [ ! -L "$iidfile" ] || die "iidfile already exists"
  for remote in CONTAINER_HOST CONTAINER_CONNECTION CONTAINER_SSHKEY DOCKER_HOST; do
    [ -z "${!remote:-}" ] || die "remote container configuration is unsupported"
  done
  podman_args=(--remote=false)
  podman "${podman_args[@]}" image inspect "$image" | python3 -c '
import json, sys
images = json.load(sys.stdin)
expected, native = sys.argv[1:]
architecture = {"aarch64": "arm64", "x86_64": "amd64"}.get(native)
if not isinstance(images, list) or len(images) != 1:
    raise SystemExit("expected one local image")
info = images[0]
if (not architecture or info.get("Os") != "linux" or info.get("Architecture") != architecture
        or info.get("Id", "").removeprefix("sha256:") != expected.removeprefix("sha256:")):
    raise SystemExit("local image identity or native architecture differs")
' "$image" "$(uname -m)" || die "immutable local image validation failed"
elif [ -n "$epoch" ]; then
  die "--source-date-epoch requires --iidfile"
else
  podman image exists "$image" || die "image is not present in local storage: $image"
fi

stage="$(mktemp -d "${TMPDIR:-/tmp}/tilo-reseal.XXXXXX")"
trap 'rm -rf "$stage"' EXIT
cp "$TILO_OS_DIR/dev/uki-cmdline.py" "$stage/uki-cmdline.py"
build_args=(-t "$image" -f - --build-arg "BASE_IMAGE=$image")
if [ -n "$iidfile" ]; then
  mapfile -t reproducible < <(podman_epoch_args "$epoch" "${podman_args[@]}")
  build_args=(-f - --build-arg "BASE_IMAGE=$image" --iidfile "$iidfile"
    --pull=never --network=none "${reproducible[@]}")
elif [ -n "${SOURCE_DATE_EPOCH:-}" ]; then
  case "$SOURCE_DATE_EPOCH" in *[!0-9]*|'') die "SOURCE_DATE_EPOCH must be an integer" ;; esac
  # Podman 5 also derives source-date-epoch from this exported variable.
  mapfile -t reproducible < <(podman_epoch_args "$SOURCE_DATE_EPOCH" "${podman_args[@]}")
  build_args+=("${reproducible[@]}")
fi
# An inherited epoch must not conflict with the explicit immutable-mode input.
if [ -n "$iidfile" ]; then export SOURCE_DATE_EPOCH="$epoch"; fi
podman "${podman_args[@]}" build "${build_args[@]}" "$stage" <<EOF
ARG BASE_IMAGE
FROM \${BASE_IMAGE}
RUN --mount=type=bind,source=uki-cmdline.py,target=/run/uki-cmdline.py \
    python3 /run/uki-cmdline.py set "composefs=$digest" \
 && if [ -f /boot/tilo-target/tilo-target.efi ]; then \
      python3 /run/uki-cmdline.py set "composefs=$digest" /boot/tilo-target/tilo-target.efi; \
    fi \
 && python3 /run/uki-cmdline.py show
EOF
if [ -n "$iidfile" ]; then
  result=$(cat "$iidfile")
  [[ "$result" =~ ^sha256:[0-9a-f]{64}$ ]] || die "reseal did not return a full immutable image ID"
fi
