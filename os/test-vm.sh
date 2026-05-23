#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEFAULT_DISK="$SCRIPT_DIR/output/qcow2/disk.qcow2"
DISK="${1:-$DEFAULT_DISK}"

if [ ! -f "$DISK" ]; then
    echo "Error: $DISK not found."
    exit 1
fi

# Try to find OVMF
OVMF="/usr/share/edk2/ovmf/OVMF_CODE.fd"
if [ ! -f "$OVMF" ]; then
    OVMF="/usr/share/OVMF/OVMF_CODE.fd"
fi

echo "Starting qOS VM with disk: $DISK"
echo "Login: quickuser / qos"

qemu-system-x86_64 \
    -enable-kvm \
    -m 4096 \
    -cpu host \
    -smp 2 \
    -drive if=pflash,format=raw,readonly=on,file="$OVMF" \
    -drive file="$DISK",format=qcow2,if=virtio \
    -display gtk,gl=on \
    -device virtio-vga-gl \
    -nic user,model=virtio-net-pci \
    -usb -device usb-tablet
