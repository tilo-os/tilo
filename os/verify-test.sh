#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

echo "=== QuickFS OS-Level Verification System ==="

# 1. Build base components
"$SCRIPT_DIR/build.sh"

# 2. Build test container image
echo "--- Building Test Container Image ---"
podman build -t localhost/tilo-test:latest -f "$SCRIPT_DIR/Containerfile.test" "$SCRIPT_DIR"

# 3. Build Test QCOW2
echo "--- Building Test QCOW2 disk image ---"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/account.env"
"$SCRIPT_DIR/dev/disk-image.sh" localhost/tilo-test:latest \
    "$SCRIPT_DIR/output-test/qcow2" --user "$TILO_ACCOUNT"

echo "=== Test Image Build Complete ==="
echo "Test QCOW2 image: $SCRIPT_DIR/output-test/qcow2/disk.qcow2"
echo ""
echo "To verify the test results:"
echo "1. Run: ./os/test-vm.sh (and login if needed)"
echo "2. Check results: journalctl -u fs-test"
echo "If you see 'All QuickFS tests passed!', the FUSE system is working perfectly."
