#!/bin/bash
# Install a composefs+UKI image onto a raw disk image, without
# bootc-image-builder.
#
#   ./os/dev/composefs/install.sh [GENERATION] [--to-disk]
#
# bib has no composefs, UKI or backend-selection surface at all, so there is
# nothing to pass it. What replaces it is the pair of commands bib is itself
# wrapping:
#
#   systemd-repart              partition + mkfs from a declarative definition
#   bootc install to-filesystem install into the mounted result
#
# --to-disk uses `bootc install to-disk --via-loopback` instead, which does its
# own partitioning. It is one command rather than thirty lines, but it sizes the
# ESP itself, and a UKI-bearing ESP has to hold two ~150 MB kernels at once for
# rollback to work — so repart is the path that scales to the real image and
# to-disk is kept as a cross-check.
#
# Root privileges are needed for loop devices and mounts; the image is built
# rootless and moved into root storage with `podman save | sudo podman load`,
# the same way os/build.sh does it.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$HERE/common.sh"

GEN=1
MODE=to-filesystem
for a in "$@"; do
  case "$a" in
    --to-disk) MODE=to-disk ;;
    --to-filesystem) MODE=to-filesystem ;;
    [0-9]*) GEN="$a" ;;
    *) die "unknown argument: $a" ;;
  esac
done

IMAGE="$QOS_CFS_IMAGE:gen$GEN"
DISK="$QOS_CFS_RUN/gen$GEN.raw"
MNT="$QOS_CFS_RUN/mnt"

mkdir -p "$QOS_CFS_RUN"
podman image exists "$IMAGE" || die "$IMAGE not built (run build.sh $GEN)"

if ! sudo -n podman image exists "$IMAGE" 2>/dev/null; then
  msg "transferring $IMAGE into root container storage"
  podman save "$IMAGE" | sudo -n podman load
fi

cleanup() {
  sudo -n umount -R "$MNT" 2>/dev/null || true
  [ -n "${LOOP:-}" ] && sudo -n losetup -d "$LOOP" 2>/dev/null || true
}
trap cleanup EXIT

msg "creating $DISK ($QOS_CFS_DISK_SIZE)"
rm -f "$DISK"
truncate -s "$QOS_CFS_DISK_SIZE" "$DISK"

# bootc runs in a container that must see the target mount, so the whole host
# root is bound in and the target addressed through it — the arrangement Fedora's
# own sealed images use.
bootc_install() {
  sudo -n podman run --rm --privileged --pid=host \
    --security-opt label=type:unconfined_t \
    -v /var/lib/containers:/var/lib/containers \
    -v /dev:/dev \
    -v /:/run/host \
    "$IMAGE" \
    bootc install "$@"
}

if [ "$MODE" = to-disk ]; then
  msg "bootc install to-disk --via-loopback --composefs-backend --bootloader systemd"
  sudo -n podman run --rm --privileged --pid=host \
    --security-opt label=type:unconfined_t \
    -v /var/lib/containers:/var/lib/containers \
    -v /dev:/dev -v "$QOS_CFS_RUN:$QOS_CFS_RUN" \
    "$IMAGE" \
    bootc install to-disk --via-loopback --composefs-backend \
      --bootloader systemd --filesystem btrfs --wipe --skip-fetch-check \
      --target-imgref "$QOS_CFS_REG_GUEST/$QOS_CFS_REPO:latest" \
      "$DISK"
else
  msg "systemd-repart: ESP 2G (vfat) + root (btrfs)"
  sudo -n systemd-repart \
    --empty=force --dry-run=no --discard=no --offline=false \
    --definitions="$HERE/repart.d" \
    --size="$QOS_CFS_DISK_SIZE" \
    "$DISK"

  LOOP="$(sudo -n losetup -fP --show "$DISK")"
  msg "loop device: $LOOP"
  sudo -n udevadm settle 2>/dev/null || true

  mkdir -p "$MNT"
  # The ESP mounts at /boot, not /boot/efi: with a UKI the loader entries and
  # the kernel live in the same place, and bootc writes /boot/EFI/Linux there.
  sudo -n mount "${LOOP}p2" "$MNT"
  sudo -n mkdir -p "$MNT/boot"
  sudo -n mount "${LOOP}p1" "$MNT/boot"
  msg "mounted: ${LOOP}p2 -> $MNT, ${LOOP}p1 -> $MNT/boot"

  msg "bootc install to-filesystem --composefs-backend --bootloader systemd"
  bootc_install to-filesystem \
    --composefs-backend \
    --bootloader systemd \
    --skip-fetch-check \
    --target-imgref "$QOS_CFS_REG_GUEST/$QOS_CFS_REPO:latest" \
    "/run/host$MNT"

  msg "installed tree:"
  sudo -n ls "$MNT" "$MNT/boot" "$MNT/boot/EFI/Linux" >&2 || true
  sudo -n du -sh "$MNT/boot" >&2 || true
  cleanup
  trap - EXIT
fi

sudo -n chown "$(id -u):$(id -g)" "$DISK"
msg "installed: $DISK"
ls -lh "$DISK" >&2
du -h --apparent-size "$DISK" >&2
