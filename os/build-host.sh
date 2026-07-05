#!/bin/bash
# Build the headless qOS host-mode bootc image.
#
# Mirrors os/build.sh but uses Containerfile.host — no compositor,
# no session, just the relay daemon + fs + a first-boot enrollment
# service. Output goes to os/output-host/qcow2/disk.qcow2.
#
# Usage:
#   ./os/build-host.sh                   # rootless container build + qcow2
#   ./os/build-host.sh --container-only  # skip the disk-image step

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"   # os/
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"                       # cargo workspace root

# Pull the digest-pinned frozen RPM repo the Containerfile expects.
# Same source of truth every OS-image build uses (see os/dev/build-image.sh
# for the desktop-image analogue).
[ -f "$SCRIPT_DIR/rpms-snapshot.env" ] && . "$SCRIPT_DIR/rpms-snapshot.env"
if [ -z "${RPMS_REF:-}" ]; then
    echo "error: no RPMS_REF — re-run the snapshot-rpms workflow (or ./os/dev/snapshot-rpms.sh) first" >&2
    exit 1
fi

echo "=== Building qOS host-mode image ==="
echo "    rpms=$RPMS_REF"

echo "--- Step 1: Container image ---"
podman build \
    --build-arg "RPMS_REF=$RPMS_REF" \
    -f "$SCRIPT_DIR/Containerfile.host" \
    -t localhost/qos-host:latest \
    "$PROJECT_DIR"
echo "Container image built: localhost/qos-host:latest"

if [[ "${1:-}" == "--container-only" ]]; then
    echo "=== Container-only build complete ==="
    exit 0
fi

echo "--- Step 2: Transferring image to root storage ---"
podman save localhost/qos-host:latest | sudo podman load
echo "Image loaded into root storage"

echo "--- Step 3: Building QCOW2 disk image ---"
mkdir -p "$SCRIPT_DIR/output-host"
sudo podman run --rm --privileged \
    --security-opt label=type:unconfined_t \
    -v "$SCRIPT_DIR/config.toml:/config.toml:ro" \
    -v "$SCRIPT_DIR/output-host:/output" \
    -v /var/lib/containers/storage:/var/lib/containers/storage \
    quay.io/centos-bootc/bootc-image-builder:latest \
    --type qcow2 \
    --rootfs btrfs \
    --chown "$(id -u):$(id -g)" \
    localhost/qos-host:latest

echo "=== Build complete ==="
echo "QCOW2 image: $SCRIPT_DIR/output-host/qcow2/disk.qcow2"
