#!/bin/bash
# Build the headless Tilo host-mode bootc image.
#
# Mirrors os/build.sh but uses Containerfile.host — no compositor,
# no session: the relay daemon, fs, and first-boot enrollment
# service. Output goes to os/output-host/qcow2/disk.qcow2.
#
# Usage:
#   ./os/build-host.sh                   # rootless container build + qcow2
#   ./os/build-host.sh --container-only  # skip the disk-image step

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"   # os/
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"                       # cargo workspace root

# Pull the digest-pinned base + frozen RPM repo the Containerfile expects.
# Same source of truth every OS-image build uses (see os/dev/build-image.sh
# for the desktop-image analogue).
[ -f "$SCRIPT_DIR/base-snapshot.env" ] && . "$SCRIPT_DIR/base-snapshot.env"
if [ -z "${BASE_REF:-}" ]; then
    echo "error: no BASE_REF — re-run the mirror-base workflow first" >&2
    exit 1
fi
if [ -z "${RPMS_REF:-}" ]; then
  RPMS_REF="$("$SCRIPT_DIR/dev/resolve-rpm-snapshot.sh")"     || { echo "build-host: resolve-rpm-snapshot.sh failed" >&2; exit 1; }
fi
if [ -z "${RPMS_REF:-}" ]; then
    echo "error: no RPMS_REF — re-run the snapshot-rpms workflow (or ./os/dev/snapshot-rpms.sh) first" >&2
    exit 1
fi

echo "=== Building Tilo host-mode image ==="
echo "    base=$BASE_REF"
echo "    rpms=$RPMS_REF"

echo "--- Step 1: Container image ---"
podman build \
    --build-arg "BASE_REF=$BASE_REF" \
    --build-arg "RPMS_REF=$RPMS_REF" \
    -f "$SCRIPT_DIR/Containerfile.host" \
    -t localhost/tilo-host:latest \
    "$PROJECT_DIR"
echo "Container image built: localhost/tilo-host:latest"

if [[ "${1:-}" == "--container-only" ]]; then
    echo "=== Container-only build complete ==="
    exit 0
fi

echo "--- Step 2: Building QCOW2 disk image ---"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/account.env"
"$SCRIPT_DIR/dev/disk-image.sh" localhost/tilo-host:latest \
    "$SCRIPT_DIR/output-host/qcow2" --user "$TILO_ACCOUNT"

echo "=== Build complete ==="
echo "QCOW2 image: $SCRIPT_DIR/output-host/qcow2/disk.qcow2"
