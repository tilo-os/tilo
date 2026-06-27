#!/bin/bash
# Build the qOS bootc image and a bootable qcow2, end to end.
#
#   ./os/dev/build-image.sh [--with-browser] [--no-cargo]
#
# Pipeline:
#   1. cargo build --release of the components, inside the build distrobox
#      (the host is immutable and lacks the Rust/C build deps).
#   2. copy the binaries into os/ (the Containerfile COPYs them).
#   3. rootless `podman build` of the bootc container image.
#   4. `podman save | sudo podman load` into root storage (bootc-image-builder
#      runs as root and reads from there).
#   5. bootc-image-builder -> os/output/qcow2/disk.qcow2.
#
# Notes / gotchas baked in from bringup:
#   * The browser (Servo) is OMITTED by default: it is unmaintained and does not
#     build against GCC 15 / C23 (glsl-optimizer 'once_flag' conflict). --with-browser
#     re-enables it and applies CFLAGS=-std=gnu17 (you must also re-add the
#     `COPY browser` lines to the Containerfile).
#   * Binaries are built in a Fedora 43 toolbox (glibc 2.42) and run on the
#     fedora-bootc:44 image (glibc newer) — forward-compatible, fine. Keep the
#     image base >= the toolbox's Fedora release.
source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"

WITH_BROWSER=0
DO_CARGO=1
for a in "$@"; do
  case "$a" in
    --with-browser) WITH_BROWSER=1 ;;
    --no-cargo)     DO_CARGO=0 ;;
    -h|--help) sed -n '2,30p' "$0"; exit 0 ;;
    *) die "unknown arg: $a" ;;
  esac
done

PKGS=(-p shell -p fs -p file-explorer -p terminal -p greeter)
BINS=(shell fs terminal file-explorer qos-greeter)
if [ "$WITH_BROWSER" = 1 ]; then PKGS+=(-p browser); BINS+=(browser); fi

if [ "$DO_CARGO" = 1 ]; then
  msg "1/5 cargo build --release (${BINS[*]}) in ${QOS_DISTROBOX:-host}"
  local_cflags=""
  [ "$WITH_BROWSER" = 1 ] && local_cflags='CFLAGS=-std=gnu17 '   # glsl-optimizer/gcc15 fix
  inenv "cd '$QOS_REPO' && ${local_cflags}cargo build --release ${PKGS[*]}"
else
  msg "1/5 skipping cargo (--no-cargo)"
fi

msg "2/5 staging binaries into os/"
REL="$QOS_REPO/target/release"
for b in "${BINS[@]}"; do
  [ -f "$REL/$b" ] || die "missing built binary: $REL/$b (run without --no-cargo)"
  cp -f "$REL/$b" "$QOS_OS_DIR/$b"
  msg "   staged $b ($(du -h "$REL/$b" | cut -f1))"
done

msg "3/5 podman build $QOS_IMAGE (rootless)"
podman build -t "$QOS_IMAGE" "$QOS_OS_DIR"

msg "4/5 transfer image to root storage"
podman save "$QOS_IMAGE" | sudo podman load

msg "5/5 bootc-image-builder -> qcow2"
mkdir -p "$QOS_OUTPUT"
rm -rf "$QOS_OUTPUT/qcow2" "$QOS_OUTPUT"/manifest* 2>/dev/null || true
sudo podman run --rm --privileged \
  --security-opt label=type:unconfined_t \
  -v "$QOS_OS_DIR/config.toml:/config.toml:ro" \
  -v "$QOS_OUTPUT:/output" \
  -v /var/lib/containers/storage:/var/lib/containers/storage \
  "$QOS_BIB" \
  --type qcow2 --rootfs btrfs --chown "$(id -u):$(id -g)" \
  "$QOS_IMAGE"

msg "DONE: $QOS_DISK"
ls -lh "$QOS_DISK"
