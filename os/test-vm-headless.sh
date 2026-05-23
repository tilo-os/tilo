#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DISK="$SCRIPT_DIR/output-test/qcow2/disk.qcow2"
LOG_FILE="$SCRIPT_DIR/test-results.log"

if [ ! -f "$DISK" ]; then
    echo "Error: $DISK not found."
    exit 1
fi

# Try to find OVMF
OVMF="/usr/share/edk2/ovmf/OVMF_CODE.fd"
if [ ! -f "$OVMF" ]; then
    OVMF="/usr/share/OVMF/OVMF_CODE.fd"
fi

echo "Starting Headless qOS VM..."
echo "Logging to $LOG_FILE"

# Run QEMU with timeout and UEFI
timeout 120s qemu-system-x86_64 \
    -enable-kvm \
    -m 4096 \
    -cpu host \
    -smp 2 \
    -drive if=pflash,format=raw,readonly=on,file="$OVMF" \
    -drive file="$DISK",format=qcow2,if=virtio \
    -nographic \
    -serial file:"$LOG_FILE" \
    -nic user,model=virtio-net-pci \
    || true

echo "VM terminated."
echo "Checking logs for success message..."

if grep -q "All QuickFS tests passed!" "$LOG_FILE"; then
    echo "✅ VERIFICATION SUCCESS: All QuickFS tests passed!"
    exit 0
else
    echo "❌ VERIFICATION FAILED: Success message not found in logs."
    echo "Tail of log file:"
    tail -n 20 "$LOG_FILE"
    exit 1
fi
