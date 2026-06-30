#!/bin/bash
# Build the qOS bootc image and a bootable qcow2, end to end.
#
#   ./os/dev/build-image.sh
#
# Pipeline:
#   1. rootless `podman build` of the bootc container image. The Rust binaries
#      are now compiled INSIDE the image (multi-stage Containerfile, `builder`
#      stage) — no host cargo build, no binary staging into os/, no F43->F44
#      glibc coupling. The build context is the REPO ROOT (the in-image compile
#      needs the workspace source); .containerignore trims target/, os/output
#      and .git.
#   2. `podman save | sudo podman load` into root storage (bootc-image-builder
#      runs as root and reads from there).
#   3. bootc-image-builder -> os/output/qcow2/disk.qcow2.
#
# Notes / gotchas baked in from bringup:
#   * First build is a long COLD Rust compile inside the container; the
#     Containerfile uses cargo + sccache cache mounts so rebuilds are fast.
#   * The browser (Servo) is OMITTED: it is unmaintained and does not build
#     against GCC 15 / C23 (glsl-optimizer 'once_flag' conflict). To re-enable,
#     add `-p browser` to the builder cargo invocation and a `COPY` in
#     os/Containerfile.
source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"

# Fail loudly: a broken container build must NOT silently fall through to imaging
# a stale container (which previously produced a misleadingly "successful" qcow2
# built from an old image). pipefail also catches `podman save | podman load`.
set -euo pipefail

for a in "$@"; do
  case "$a" in
    -h|--help) sed -n '2,30p' "$0"; exit 0 ;;
    *) die "unknown arg: $a (host cargo build is gone; binaries compile in-image)" ;;
  esac
done

# Reproducible builds: derive a deterministic SOURCE_DATE_EPOCH from the HEAD
# commit time (same commit -> same value), pass it through to the build, and
# clamp layer + image timestamps to it. With the base pinned by digest and the
# Rust resolve pinned (os/Cargo.lock.pinned), two builds of the same commit
# yield the same image (modulo dnf package drift over time — see
# docs/supply-chain.md). Override via $SOURCE_DATE_EPOCH.
SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git -C "$QOS_REPO" log -1 --format=%ct 2>/dev/null || echo 0)}"
export SOURCE_DATE_EPOCH
msg "1/3 podman build $QOS_IMAGE (rootless, in-image cargo compile; SOURCE_DATE_EPOCH=$SOURCE_DATE_EPOCH)"
podman build -f "$QOS_OS_DIR/Containerfile" -t "$QOS_IMAGE" \
  --build-arg "SOURCE_DATE_EPOCH=$SOURCE_DATE_EPOCH" \
  --source-date-epoch "$SOURCE_DATE_EPOCH" --rewrite-timestamp \
  "$QOS_REPO"

msg "2/3 transfer image to root storage"
podman save "$QOS_IMAGE" | sudo podman load

msg "3/3 bootc-image-builder -> qcow2"
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
