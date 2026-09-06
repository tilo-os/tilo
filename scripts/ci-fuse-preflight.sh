#!/usr/bin/env bash
set -euo pipefail

# Brokered Kata runners expose the guest kernel's misc device, but a fresh
# nested job container may not carry its /dev node. Recreate that node from
# the kernel-owned major/minor; this still fails closed when the running
# kernel has no real FUSE device.
if [[ ! -c /dev/fuse && -r /sys/class/misc/fuse/dev ]]; then
    IFS=: read -r fuse_major fuse_minor < /sys/class/misc/fuse/dev
    mknod -m 0666 /dev/fuse c "$fuse_major" "$fuse_minor"
fi

if [[ ! -c /dev/fuse ]]; then
    ls -l /dev/fuse 2>&1 || true
    echo "runner did not expose the FUSE character device" >&2
    exit 1
fi

if [[ ! -r /dev/fuse || ! -w /dev/fuse ]]; then
    ls -l /dev/fuse 2>&1 || true
    echo "runner exposed /dev/fuse without read/write access" >&2
    exit 1
fi

fuse_helper="$(command -v fusermount3)" || {
    echo "qOS builder image does not contain fusermount3" >&2
    exit 1
}

ls -l /dev/fuse
echo "FUSE preflight ready: ${fuse_helper}"
