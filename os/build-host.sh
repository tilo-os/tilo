#!/bin/bash
# Build the headless Tilo server image (os/Containerfile.host) and, unless
# --container-only, a bootable qcow2 from it. Output goes to
# os/output-host/qcow2/disk.qcow2.
#
#   ./os/build-host.sh                   # image + disk
#   ./os/build-host.sh --container-only  # image only
#
# It takes the same steps CI takes (tools/ci/publish/host-base.sh, then
# host-build.sh over the programs server-binaries.sh host compiles and the bootc
# os-tools-x86_64 built) rather than a second recipe of its own, so a local build
# and a published one are the same image. The disk step is os/dev/build-image.sh's
# install, which seals the UKI's composefs digest as CI's seal-host does. The
# generation is the next of the server's published series, or TILO_GENERATION.
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

echo "--- Step 2: the base ---"
bash "$PROJECT_DIR/tools/ci/publish/host-base.sh" "$snapshot" "$OUT/rpms" "$OUT/base"

echo "--- Step 3: the bootc the OS image's tools stage built ---"
# One build for both images: the server image installs the bootc the OS tools check
# (os-tools-x86_64) built from os/external-assets/bootc-runtime/, which t serves from
# its cache while the base, the frozen snapshot and the patches stand. TILO_OS_TOOLS
# names a layout already at hand, for a machine without t.
tools=${TILO_OS_TOOLS:-}
if [ -z "$tools" ]; then
  command -v t >/dev/null \
    || { echo "no t to fetch os-tools-x86_64's bootc; set TILO_OS_TOOLS to its output directory (tools/)" >&2; exit 2; }
  t --dir "$PROJECT_DIR" outputs os-tools-x86_64 --to "$OUT/os-tools"
  tools=$OUT/os-tools/target/os/x86_64/tools
fi

echo "--- Step 4: the image ---"
if [ -n "${TILO_GENERATION:-}" ]; then
  printf '%s\n' "$TILO_GENERATION" > "$OUT/generation"
else
  TILO_PUBLISH_REPO=registry.tilo.so/tilo/tilo-host TILO_ALLOW_EMPTY_SERIES=1 \
    "$SCRIPT_DIR/dev/next-generation.sh" > "$OUT/generation"
fi
bash "$PROJECT_DIR/tools/ci/publish/host-build.sh" "$OUT/binaries" "$OUT/base" "$tools" "$OUT/generation" "$OUT/image"

if [ "$container_only" = 1 ]; then
  echo "=== Image only: $OUT/image/image.oci.tar ==="
  exit 0
fi

echo "--- Step 5: the disk image ---"
image=$(podman pull -q "oci-archive:$OUT/image/image.oci.tar")
podman tag "$image" localhost/tilo-host:latest
TILO_IMAGE=localhost/tilo-host:latest TILO_SKIP_BUILD=1 TILO_OUTPUT="$OUT" \
  "$SCRIPT_DIR/dev/build-image.sh"
echo "=== Build complete: $OUT/qcow2/disk.qcow2 ==="
