#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

echo "=== QuickFS OS-Level Verification System ==="

# 1. Build base components
"$SCRIPT_DIR/build.sh"

# 2. Build test container image
echo "--- Building Test Container Image ---"
podman build -t localhost/qos-test:latest -f "$SCRIPT_DIR/Containerfile.test" "$SCRIPT_DIR"

# 3. Load into root storage for builder
echo "--- Transferring test image to root storage ---"
podman save localhost/qos-test:latest | sudo podman load

# 4. Build Test QCOW2
echo "--- Building Test QCOW2 disk image ---"
mkdir -p "$SCRIPT_DIR/output-test"
sudo podman run --rm --privileged \
    --security-opt label=type:unconfined_t \
    -v "$SCRIPT_DIR/config.toml:/config.toml:ro" \
    -v "$SCRIPT_DIR/output-test:/output" \
    -v /var/lib/containers/storage:/var/lib/containers/storage \
    quay.io/centos-bootc/bootc-image-builder:latest \
    --type qcow2 \
    --rootfs btrfs \
    --chown "$(id -u):$(id -g)" \
    localhost/qos-test:latest

echo "=== Test Image Build Complete ==="
echo "Test QCOW2 image: $SCRIPT_DIR/output-test/qcow2/disk.qcow2"
echo ""
echo "To verify the test results:"
echo "1. Run: ./os/test-vm.sh (and login if needed)"
echo "2. Check results: journalctl -u fs-test"
echo "If you see 'All QuickFS tests passed!', the FUSE system is working perfectly."
