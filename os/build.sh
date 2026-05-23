#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

echo "=== Building qOS Image ==="

# Step 1: Build the compositor
echo "--- Step 1: Building compositor ---"
cd "$PROJECT_DIR/shell"
cargo build --release
cp target/release/shell "$SCRIPT_DIR/shell"
echo "Compositor binary copied to os/shell"

# Step 1.5: Build the FUSE filesystem
echo "--- Step 1.5: Building FUSE filesystem ---"
cd "$PROJECT_DIR/fs"
cargo build --release
cp target/release/fs "$SCRIPT_DIR/fs"
echo "FUSE binary copied to os/fs"

# Step 1.7: Build the applications
echo "--- Step 1.7: Building applications ---"
cd "$PROJECT_DIR"
cargo build --release -p file-explorer -p terminal -p browser
cp target/release/file-explorer "$SCRIPT_DIR/file-explorer"
cp target/release/terminal "$SCRIPT_DIR/terminal"
cp target/release/browser "$SCRIPT_DIR/browser"
echo "Application binaries copied to os/"

# Step 2: Build the container image (as user)
echo "--- Step 2: Building container image ---"
cd "$SCRIPT_DIR"
podman build -t localhost/qos:latest .
echo "Container image built: localhost/qos:latest"

# Step 3: Transfer image to root storage (bootc-image-builder runs as root)
echo "--- Step 3: Transferring image to root storage ---"
podman save localhost/qos:latest | sudo podman load
echo "Image loaded into root storage"

# Step 4: Build QCOW2 disk image
echo "--- Step 4: Building QCOW2 disk image ---"
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
