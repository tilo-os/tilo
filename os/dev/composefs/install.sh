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
# ESP itself and a UKI-bearing ESP has to hold two ~150 MB kernels at once for
# rollback to work — so repart is the path that scales, and to-disk is kept as a
# cross-check.
#
# The source has to be a registry, not root container storage. Installing from
# `containers-storage:` — including an image moved across with
# `podman save | sudo podman load`, which is what os/build.sh does — fails in
# the composefs pull with
#
#   Pulling image into composefs repository: Opening stream '':
#   Creating new splitstream reader: Invalid splitstream content type
#
# Pulling the same image over HTTP works. That is a constraint on replacing
# bib: the disk step needs a registry the installer can reach, so a local one is
# started here rather than assumed.
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

IMAGE="$TILO_CFS_IMAGE:gen$GEN"
DISK="$TILO_CFS_RUN/gen$GEN.raw"
MNT="$TILO_CFS_RUN/mnt"

mkdir -p "$TILO_CFS_RUN"
podman image exists "$IMAGE" || die "$IMAGE not built (run build.sh $GEN)"

# --- a registry the installer can pull from ----------------------------------
if ! podman container exists tilo-cfs-registry 2>/dev/null; then
  msg "starting a registry on :$TILO_CFS_REG_PORT"
  podman run -d --name tilo-cfs-registry \
    -p "$TILO_CFS_REG_PORT:5000" docker.io/library/registry:2 >/dev/null
elif [ "$(podman inspect -f '{{.State.Running}}' tilo-cfs-registry)" != true ]; then
  podman start tilo-cfs-registry >/dev/null
fi
for _ in $(seq 1 30); do
  curl -sf "http://$TILO_CFS_REG_HOST/v2/" >/dev/null && break
  sleep 1
done
curl -sf "http://$TILO_CFS_REG_HOST/v2/" >/dev/null \
  || die "registry did not come up on $TILO_CFS_REG_HOST"

msg "publishing $IMAGE as $TILO_CFS_REG_HOST/$TILO_CFS_REPO:latest"
podman push --tls-verify=false "$IMAGE" "$TILO_CFS_REG_HOST/$TILO_CFS_REPO:latest"

# The installing container needs the registry marked insecure. The image carries
# a drop-in for the address the *guest* uses (10.0.2.2:5000); this adds the one
# the host uses.
REGCONF="$TILO_CFS_RUN/98-local-registry.conf"
printf '[[registry]]\nlocation = "%s"\ninsecure = true\n' "$TILO_CFS_REG_HOST" > "$REGCONF"

# bootc runs *out of* the image being installed, so root podman needs it too.
# Rather than move it across with save|load — which is the transfer that breaks
# the composefs pull — run the copy that is already in the registry. One
# reference then serves as both the container to run and the install source.
RUNREF="$TILO_CFS_REG_HOST/$TILO_CFS_REPO:latest"

cleanup() {
  sudo -n umount -R "$MNT" 2>/dev/null || true
  [ -n "${LOOP:-}" ] && sudo -n losetup -d "$LOOP" 2>/dev/null || true
}
trap cleanup EXIT

msg "creating $DISK ($TILO_CFS_DISK_SIZE)"
rm -f "$DISK"
truncate -s "$TILO_CFS_DISK_SIZE" "$DISK"

if [ "$MODE" = to-disk ]; then
  msg "bootc install to-disk --via-loopback --composefs-backend --bootloader systemd"
  sudo -n podman run --rm --privileged --pid=host --network host \
    --security-opt label=type:unconfined_t \
    -v /var/lib/containers:/var/lib/containers \
    -v /dev:/dev -v "$TILO_CFS_RUN:$TILO_CFS_RUN" \
    -v "$REGCONF:/etc/containers/registries.conf.d/98-local.conf:ro" \
    --tls-verify=false "$RUNREF" \
    bootc install to-disk --via-loopback --composefs-backend \
      --bootloader systemd --filesystem btrfs --wipe --skip-fetch-check \
      --source-imgref "docker://$TILO_CFS_REG_HOST/$TILO_CFS_REPO:latest" \
      --target-imgref "$TILO_CFS_REG_GUEST/$TILO_CFS_REPO:latest" \
      "$DISK"
else
  msg "systemd-repart: ESP 2G (vfat) + root (btrfs)"
  sudo -n systemd-repart \
    --empty=force --dry-run=no --discard=no --offline=false \
    --sector-size=512 --definitions="$HERE/repart.d" \
    --size="$TILO_CFS_DISK_SIZE" \
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
  # /:/run/host so the container can see a mount made in the host namespace,
  # which is how Fedora's own sealed images do it.
  sudo -n podman run --rm --privileged --pid=host --network host \
    --security-opt label=type:unconfined_t \
    -v /var/lib/containers:/var/lib/containers \
    -v /dev:/dev -v /:/run/host \
    -v "$REGCONF:/etc/containers/registries.conf.d/98-local.conf:ro" \
    --tls-verify=false "$RUNREF" \
    bootc install to-filesystem \
      --composefs-backend \
      --bootloader systemd \
      --skip-fetch-check \
      --source-imgref "docker://$TILO_CFS_REG_HOST/$TILO_CFS_REPO:latest" \
      --target-imgref "$TILO_CFS_REG_GUEST/$TILO_CFS_REPO:latest" \
      "/run/host$MNT"

  msg "installed tree:"
  sudo -n ls "$MNT" "$MNT/boot" "$MNT/boot/EFI/Linux" >&2 || true
  sudo -n du -sh "$MNT/boot" >&2 || true
fi

cleanup
trap - EXIT
sudo -n chown "$(id -u):$(id -g)" "$DISK"
msg "installed: $DISK"
ls -lh "$DISK" >&2
