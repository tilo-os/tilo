#!/bin/bash
# Build the Tilo dev image (os/Containerfile.dev) and a bootable dev VM disk.
#
#   ./os/dev/build-dev-image.sh          (or ./os/build.sh --dev)
#
# Pipeline, the same as the CI lane .forgejo/workflows/dev-disk.yml:
#   1. rootless `podman build -f os/Containerfile.dev` FROM a production image
#      that is already in local storage. The dev binaries compile inside the
#      build, with the production builder's toolchain.
#   2. os/dev/build-image.sh with TILO_SKIP_BUILD=1 installs that image to
#      $TILO_OUTPUT/qcow2/disk.qcow2. The dev image is unsealed, so this is a
#      single `bootc install` pass with ordinary boot entries. This step needs
#      passwordless sudo (`sudo -n`) for the loop device and mounts; without it
#      the script stops after step 1 and prints the command to run later.
#
# Environment:
#   TILO_DEV_BASE     production image to build FROM (default localhost/tilo:latest,
#                     what ./os/build.sh produces). A CI image works too:
#                       podman pull code.podesta.ai/qos/tilo:<tag>
#                       TILO_DEV_BASE=code.podesta.ai/qos/tilo:<tag> ./os/dev/build-dev-image.sh
#   TILO_DEV_IMAGE    tag for the dev image (default localhost/tilo-dev:latest);
#                     must name tilo-dev, never the production image
#   TILO_DEV_DISK     0 builds the container only (default 1)
#   TILO_OUTPUT       disk output root (default os/output/dev)
#   TILO_BUILD_ARGS   extra `podman build` flags, e.g. --no-cache
#   TILO_TARGET_IMGREF, TILO_DISK_SIZE, TILO_ACCOUNT  passed to the disk step
set -euo pipefail

for a in "$@"; do
  case "$a" in
    -h|--help) sed -n '2,27p' "$0"; exit 0 ;;
    *) printf 'build-dev-image: unknown arg: %s\n' "$a" >&2; exit 2 ;;
  esac
done

_dev_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export TILO_OUTPUT="${TILO_OUTPUT:-$(cd "$_dev_dir/.." && pwd)/output/dev}"
export TILO_DISK="${TILO_DISK:-$TILO_OUTPUT/qcow2/disk.qcow2}"
source "$_dev_dir/_common.sh"

base="${TILO_DEV_BASE:-localhost/tilo:latest}"
dev_image="${TILO_DEV_IMAGE:-localhost/tilo-dev:latest}"
case "$dev_image" in
  localhost/tilo-dev:*|code.podesta.ai/qos/tilo-dev:*) ;;
  *) die "TILO_DEV_IMAGE=$dev_image: the dev image may only be tagged localhost/tilo-dev:<tag> or
  code.podesta.ai/qos/tilo-dev:<tag>. It runs sshd and dev-overrides binaries, so it must never
  carry a production image name." ;;
esac

python3 "$TILO_REPO/tools/policy/check_dev_image_contract.py" >/dev/null \
  || { python3 "$TILO_REPO/tools/policy/check_dev_image_contract.py" || true; die "fix the dev image contract first (see above)"; }

podman image exists "$base" || die "the production base $base is not in local storage.
  Build it with ./os/build.sh, or use a CI image:
    podman pull code.podesta.ai/qos/tilo:<tag>
    TILO_DEV_BASE=code.podesta.ai/qos/tilo:<tag> $0"
base_digest="$(podman image inspect --format '{{.Digest}}' "$base" 2>/dev/null || true)"
[[ "$base_digest" =~ ^sha256:[0-9a-f]{64}$ ]] || base_digest=""

read -r -a _extra_build_args <<< "${TILO_BUILD_ARGS:-}"
SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git -C "$TILO_REPO" log -1 --format=%ct 2>/dev/null || echo 0)}"
export SOURCE_DATE_EPOCH
: "${TMPDIR:=$TILO_OUTPUT/.tmp}"
mkdir -p "$TMPDIR"
export TMPDIR

msg "1/2 podman build $dev_image FROM $base${base_digest:+ ($base_digest)}"
podman build -f "$TILO_OS_DIR/Containerfile.dev" \
  --build-arg "BASE_IMAGE=$base" \
  --build-arg "TILO_BASE_DIGEST=$base_digest" \
  --build-arg "SOURCE_DATE_EPOCH=$SOURCE_DATE_EPOCH" \
  "${_extra_build_args[@]}" \
  --tag "$dev_image" \
  "$TILO_REPO"

disk_command="TILO_IMAGE=$dev_image TILO_SKIP_BUILD=1 TILO_OUTPUT=$TILO_OUTPUT $TILO_OS_DIR/dev/build-image.sh"
if [ "${TILO_DEV_DISK:-1}" = 0 ]; then
  msg "DONE: $dev_image (TILO_DEV_DISK=0: no disk). Write one later with:
  $disk_command"
  exit 0
fi
sudo -n true 2>/dev/null || die "the disk step needs passwordless sudo (sudo -n) for its loop device and
  mounts. $dev_image is built; write the disk from a shell that has it:
  $disk_command"

msg "2/2 disk image of $dev_image -> $TILO_DISK"
TILO_IMAGE="$dev_image" TILO_SKIP_BUILD=1 exec "$TILO_OS_DIR/dev/build-image.sh"
