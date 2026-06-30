#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"   # os/
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"                       # cargo workspace root

echo "=== Building qOS Image ==="

# Step 1: Build the container image.
#
# The Rust binaries (shell, fs, terminal, file-explorer, qos-greeter,
# qos-installer) are now compiled INSIDE the image via the multi-stage
# Containerfile (`builder` stage) — there is no host cargo build and no binary
# staging into os/. The build context is the REPO ROOT so the in-image compile
# can see the whole cargo workspace; .containerignore trims target/, os/output
# and .git. The first build is a long cold Rust compile (cached on rebuilds).
echo "--- Step 1: Building container image (in-image cargo compile) ---"
podman build -f "$SCRIPT_DIR/Containerfile" -t localhost/qos:latest "$PROJECT_DIR"
echo "Container image built: localhost/qos:latest"

# Step 2: Transfer image to root storage (bootc-image-builder runs as root)
echo "--- Step 2: Transferring image to root storage ---"
podman save localhost/qos:latest | sudo podman load
echo "Image loaded into root storage"

# Step 3: Build QCOW2 disk image
echo "--- Step 3: Building QCOW2 disk image ---"
mkdir -p "$SCRIPT_DIR/output"
sudo podman run --rm --privileged \
    --security-opt label=type:unconfined_t \
    -v "$SCRIPT_DIR/config.toml:/config.toml:ro" \
    -v "$SCRIPT_DIR/output:/output" \
    -v /var/lib/containers/storage:/var/lib/containers/storage \
    quay.io/centos-bootc/bootc-image-builder:latest \
    --type qcow2 \
    --rootfs btrfs \
    --chown "$(id -u):$(id -g)" \
    localhost/qos:latest

echo "=== Build complete ==="
echo "QCOW2 image: $SCRIPT_DIR/output/qcow2/disk.qcow2"
echo "Run ./test-vm.sh to test in QEMU"
