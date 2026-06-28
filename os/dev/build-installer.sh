#!/bin/bash
# Build a qOS INSTALLER ISO (bootc-image-builder --type anaconda-iso) that
# embeds the already-built qOS image and installs it to a target disk via
# Anaconda. Reuses the localhost/qos image produced by build-image.sh.
#
#   ./os/dev/build-image.sh          # build + load the qOS image first
#   ./os/dev/build-installer.sh      # -> os/output/bootiso/install.iso
#   ./os/dev/vm.sh boot-iso          # boot the installer in QEMU
#
# Config (interactive vs unattended) lives in os/installer.toml.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"

# The ISO embeds the qOS image, which must be in ROOT containers-storage
# (that's where bootc-image-builder reads it). build-image.sh's step 4
# (`podman save | sudo podman load`) put it there.
if ! sudo podman image exists "$QOS_IMAGE"; then
  die "image $QOS_IMAGE not in root storage — run ./os/dev/build-image.sh first"
fi

msg "bootc-image-builder -> anaconda installer ISO (this is slow)"
mkdir -p "$QOS_OUTPUT"
rm -rf "$QOS_OUTPUT/bootiso" 2>/dev/null || true
sudo podman run --rm --privileged \
  --security-opt label=type:unconfined_t \
  -v "$QOS_OS_DIR/installer.toml:/config.toml:ro" \
  -v "$QOS_OUTPUT:/output" \
  -v /var/lib/containers/storage:/var/lib/containers/storage \
  "$QOS_BIB" \
  --type anaconda-iso --rootfs btrfs --chown "$(id -u):$(id -g)" \
  "$QOS_IMAGE"

ISO="$(find "$QOS_OUTPUT" -name '*.iso' -type f 2>/dev/null | head -1)"
msg "DONE: ${ISO:-<no .iso found under $QOS_OUTPUT>}"
[ -n "$ISO" ] && ls -lh "$ISO"
