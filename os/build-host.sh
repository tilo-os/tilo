#!/bin/bash
# Build the headless Tilo server image (os/Containerfile.host) and, unless
# --container-only, a bootable qcow2 from it. Output goes to
# os/output-host/qcow2/disk.qcow2.
#
#   ./os/build-host.sh                   # image + disk
#   ./os/build-host.sh --container-only  # image only
#
# It runs the same two steps CI runs (tools/ci/publish/host-build.sh, over the
# programs server-binaries.sh host compiles) rather than a second recipe of its
# own, so a local build and a published one are the same image. The disk step is
# os/dev/disk-image.sh, which the installer and every other Tilo disk uses.
#
# usage: see above.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"   # os/
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"                       # cargo workspace root
OUT="$SCRIPT_DIR/output-host"

container_only=0
case "${1:-}" in
  --container-only) container_only=1 ;;
  '') ;;
  *) echo "usage: $0 [--container-only]" >&2; exit 2 ;;
esac

# The frozen RPM snapshot every Tilo image installs from (see
# os/dev/resolve-rpm-snapshot.sh); the base is pinned in os/base-snapshot.env.
snapshot=$OUT/rpm-snapshot
mkdir -p "$snapshot"
if [ ! -s "$snapshot/rpms-ref" ]; then
  "$SCRIPT_DIR/dev/resolve-rpm-snapshot.sh" > "$snapshot/rpms-ref"
fi

echo "--- Step 1: the server's programs ---"
bash "$PROJECT_DIR/tools/ci/publish/server-binaries.sh" host "$OUT/binaries"

echo "--- Step 2: the image ---"
bash "$PROJECT_DIR/tools/ci/publish/host-build.sh" "$snapshot" "$OUT/rpms" "$OUT/binaries" "$OUT/image"

if [ "$container_only" = 1 ]; then
  echo "=== Image only: $OUT/image/image.oci.tar ==="
  exit 0
fi

echo "--- Step 3: the disk image ---"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/account.env"
image=$(podman pull -q "oci-archive:$OUT/image/image.oci.tar")
podman tag "$image" localhost/tilo-host:latest
"$SCRIPT_DIR/dev/disk-image.sh" localhost/tilo-host:latest \
    "$OUT/qcow2" --user "$TILO_ACCOUNT"
echo "=== Build complete: $OUT/qcow2/disk.qcow2 ==="
