#!/bin/bash
# Turn a bootc container image into a bootable qcow2 — the job
# bootc-image-builder used to do here.
#
#   ./os/dev/disk-image.sh IMAGE OUTDIR [options]
#
#     --composefs             composefs backend (implied when the image has a UKI)
#     --bootloader systemd|grub|none
#     --size 20G              whole-disk size
#     --root-min 12G          minimum root partition size
#     --esp 2G                ESP size
#     --karg K=V              extra kernel argument (repeatable)
#     --user NAME:PASS[:G,G]  create a login account in the installed system
#     --raw                   leave the raw image, skip the qcow2 conversion
#
# Why not bib: bib has no composefs, UKI or backend-selection surface anywhere
# in its config, and the request for one (osbuild/bootc-image-builder#1164) has
# been idle since December. What bib does at this step is wrap the two commands
# below, so calling them directly is less machinery rather than more:
#
#   systemd-repart               partition + mkfs from a declarative definition
#   bootc install to-filesystem  install into the mounted result
#
# What bib also provided, and how it is replaced:
#
#   --type qcow2       qemu-img convert at the end
#   --rootfs btrfs     Format=btrfs in the repart definition
#   --chown            chown at the end
#   customizations.user        --user (chroot useradd into the installed root)
#   customizations.kernel      --karg, but see the note on sealed images below
#   customizations.filesystem  --root-min / --esp
#
# Sealed images (composefs + UKI): the kernel command line is signed into the
# UKI at *build* time, so --karg cannot reach it. A karg that has to be on the
# command line of a sealed image belongs in that image's
# /usr/lib/bootc/kargs.d/ — which is where `bootc container ukify` reads it
# from — or in a separately signed UKI addon. --karg still works for unsealed
# installs, and is passed through unchanged.
set -euo pipefail

source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"

IMAGE=""; OUTDIR=""
COMPOSEFS=auto; BOOTLOADER=""; SIZE=20G; ROOT_MIN=12G; ESP_SIZE=2G
KARGS=(); USERSPEC=""; KEEP_RAW=0

while [ $# -gt 0 ]; do
  case "$1" in
    --composefs) COMPOSEFS=yes ;;
    --no-composefs) COMPOSEFS=no ;;
    --bootloader) BOOTLOADER="$2"; shift ;;
    --size) SIZE="$2"; shift ;;
    --root-min) ROOT_MIN="$2"; shift ;;
    --esp) ESP_SIZE="$2"; shift ;;
    --karg) KARGS+=("--karg" "$2"); shift ;;
    --user) USERSPEC="$2"; shift ;;
    --raw) KEEP_RAW=1 ;;
    -h|--help) sed -n '2,40p' "$0"; exit 0 ;;
    -*) die "unknown option: $1" ;;
    *) if [ -z "$IMAGE" ]; then IMAGE="$1"; elif [ -z "$OUTDIR" ]; then OUTDIR="$1";
       else die "unexpected argument: $1"; fi ;;
  esac
  shift
done
[ -n "$IMAGE" ] && [ -n "$OUTDIR" ] || { sed -n '2,40p' "$0"; exit 1; }

podman image exists "$IMAGE" || die "$IMAGE is not in rootless podman storage"

# A UKI in the image means bootc selects the composefs backend by itself, and
# the install must agree — so detect rather than make the caller remember.
if [ "$COMPOSEFS" = auto ]; then
  if podman run --rm --entrypoint /bin/sh "$IMAGE" \
       -c 'ls /boot/EFI/Linux/*.efi >/dev/null 2>&1'; then
    COMPOSEFS=yes
    msg "the image carries a UKI — using the composefs backend"
  else
    COMPOSEFS=no
  fi
fi

mkdir -p "$OUTDIR"
RAW="$OUTDIR/disk.raw"
QCOW="$OUTDIR/disk.qcow2"
MNT="$OUTDIR/.mnt"
DEFS="$OUTDIR/.repart.d"

cleanup() {
  sudo -n umount -R "$MNT" 2>/dev/null || true
  [ -n "${LOOP:-}" ] && sudo -n losetup -d "$LOOP" 2>/dev/null || true
  rm -rf "$DEFS"
}
trap cleanup EXIT

if ! sudo -n podman image exists "$IMAGE" 2>/dev/null; then
  msg "transfer $IMAGE into root container storage"
  podman save "$IMAGE" | sudo -n podman load
fi

# The ESP has to hold a UKI per generation, not just a bootloader: two ~150 MB
# kernels at once is the working set an update plus its rollback needs.
mkdir -p "$DEFS"
printf '[Partition]\nType=esp\nFormat=vfat\nSizeMinBytes=%s\nSizeMaxBytes=%s\n' \
  "$ESP_SIZE" "$ESP_SIZE" > "$DEFS/01-esp.conf"
printf '[Partition]\nType=root\nFormat=btrfs\nSizeMinBytes=%s\nGrowFileSystem=yes\n' \
  "$ROOT_MIN" > "$DEFS/02-root.conf"

msg "1/4 systemd-repart -> $RAW (ESP $ESP_SIZE + btrfs root, disk $SIZE)"
rm -f "$RAW"
truncate -s "$SIZE" "$RAW"
sudo -n systemd-repart --empty=force --dry-run=no --discard=no --offline=false \
  --definitions="$DEFS" --size="$SIZE" "$RAW"

LOOP="$(sudo -n losetup -fP --show "$RAW")"
sudo -n udevadm settle 2>/dev/null || true
mkdir -p "$MNT"
sudo -n mount "${LOOP}p2" "$MNT"
sudo -n mkdir -p "$MNT/boot"
# With a UKI the kernel and the loader entries live in the same place, so the
# ESP mounts at /boot. On the ostree layout /boot is its own filesystem and the
# ESP sits under it — but bib's layout had no separate /boot either, so a single
# ESP-at-/boot arrangement covers both.
sudo -n mount "${LOOP}p1" "$MNT/boot"
msg "2/4 mounted ${LOOP}p2 -> $MNT, ${LOOP}p1 -> $MNT/boot"

install_args=(to-filesystem --skip-fetch-check)
[ "$COMPOSEFS" = yes ] && install_args+=(--composefs-backend)
[ -n "$BOOTLOADER" ] && install_args+=(--bootloader "$BOOTLOADER")
install_args+=("${KARGS[@]+"${KARGS[@]}"}")
install_args+=("/run/host$MNT")

msg "3/4 bootc install ${install_args[*]}"
sudo -n podman run --rm --privileged --pid=host \
  --security-opt label=type:unconfined_t \
  -v /var/lib/containers:/var/lib/containers \
  -v /dev:/dev -v /:/run/host \
  "$IMAGE" bootc install "${install_args[@]}"

# bib's customizations.user, replaced. The installed root's /etc is a real
# directory on the target filesystem for both backends, so useradd run inside
# the image against it does the same thing bib's stage did.
if [ -n "$USERSPEC" ]; then
  IFS=: read -r u_name u_pass u_groups <<< "$USERSPEC"
  etc="$(sudo -n sh -c "ls -d $MNT/state/deploy/*/etc 2>/dev/null | head -1")"
  [ -n "$etc" ] || etc="$(sudo -n sh -c "ls -d $MNT/ostree/deploy/*/deploy/*/etc 2>/dev/null | head -1")"
  [ -n "$etc" ] || die "could not find the installed system's /etc under $MNT"
  msg "creating user $u_name in $etc"
  sudo -n podman run --rm --privileged \
    -v "$etc:/target-etc" "$IMAGE" \
    bash -c "set -e
      mount --bind /target-etc /etc
      useradd -m ${u_groups:+-G '$u_groups'} '$u_name'
      printf '%s:%s\n' '$u_name' '$u_pass' | chpasswd"
fi

cleanup
trap - EXIT

if [ "$KEEP_RAW" = 1 ]; then
  msg "4/4 leaving the raw image in place"
  sudo -n chown "$(id -u):$(id -g)" "$RAW"
  msg "DONE: $RAW"
else
  msg "4/4 qemu-img convert -> $QCOW"
  sudo -n chown "$(id -u):$(id -g)" "$RAW"
  inenv "qemu-img convert -f raw -O qcow2 '$RAW' '$QCOW'"
  rm -f "$RAW"
  msg "DONE: $QCOW"
  ls -lh "$QCOW" >&2
fi
