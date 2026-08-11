#!/bin/bash
# Install a composefs+UKI image onto a raw disk image — without
# bootc-image-builder.
#
#   ./os/dev/composefs/install.sh [GENERATION]
#
# bib has no composefs, UKI or backend-selection surface at all, so there is
# nothing to pass it. What replaces it is `bootc install to-disk --via-loopback`
# run out of the image itself: bootc does its own partitioning, mkfs, ESP setup
# and bootloader install, and writes the result straight into a file. That is
# strictly less machinery than bib (which wraps osbuild around the same bootc
# install call) — the cost of dropping bib is in the *surrounding* features bib
# also provides, not in this step.
#
# Root podman is used for the install because it needs loop devices and mounts.
# The image is built rootless and transferred with `podman save | sudo podman
# load`, which is what os/build.sh already does.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$HERE/common.sh"

GEN="${1:-1}"
IMAGE="$QOS_CFS_IMAGE:gen$GEN"
DISK="$QOS_CFS_RUN/gen$GEN.raw"

mkdir -p "$QOS_CFS_RUN"

podman image exists "$IMAGE" || die "$IMAGE not built (run build.sh $GEN)"

if ! sudo -n podman image exists "$IMAGE" 2>/dev/null; then
  msg "transferring $IMAGE into root container storage"
  podman save "$IMAGE" | sudo -n podman load
fi

msg "creating $DISK ($QOS_CFS_DISK_SIZE)"
rm -f "$DISK"
truncate -s "$QOS_CFS_DISK_SIZE" "$DISK"

# --target-imgref points updates at the registry running on the VM host; the
# guest resolves that name through user-mode networking.
msg "bootc install to-disk --composefs-backend --bootloader systemd (no bib)"
sudo -n podman run --rm --privileged --pid=host \
  --security-opt label=type:unconfined_t \
  -v /var/lib/containers:/var/lib/containers \
  -v /dev:/dev \
  -v "$QOS_CFS_RUN:$QOS_CFS_RUN" \
  "$IMAGE" \
  bootc install to-disk \
    --via-loopback \
    --composefs-backend \
    --bootloader systemd \
    --filesystem btrfs \
    --wipe \
    --skip-fetch-check \
    --target-imgref "$QOS_CFS_REG_GUEST/$QOS_CFS_REPO:latest" \
    "$DISK"

sudo -n chown "$(id -u):$(id -g)" "$DISK"
msg "installed: $DISK"
ls -lh "$DISK" >&2
