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
#   * The browser is built in-image (`-p browser --features browser/wpe`) and
#     its WPE WebKit engine + runtime libs are compiled in the `wpe-builder`
#     stage and copied into the final image (see os/Containerfile). The old
#     Servo browser is gone.
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
# The digest-pinned frozen RPM repo the build installs from (written by
# os/dev/snapshot-rpms.sh / the snapshot-rpms CI job). Cross-time reproducible.
# shellcheck disable=SC1091
[ -f "$QOS_OS_DIR/rpms-snapshot.env" ] && . "$QOS_OS_DIR/rpms-snapshot.env"
[ -n "${RPMS_REF:-}" ] || die "no RPMS_REF — run ./os/dev/snapshot-rpms.sh (or the snapshot-rpms CI job) first"
# The digest-pinned qos-kernel image (built by .forgejo/workflows/build-kernel.yml
# — see os/kernel/Containerfile).  os/Containerfile swaps kernel-core →
# qos-kernel-core out of it.  Cross-time reproducible.
# shellcheck disable=SC1091
[ -f "$QOS_OS_DIR/kernel-snapshot.env" ] && . "$QOS_OS_DIR/kernel-snapshot.env"
[ -n "${KERNEL_RPMS_REF:-}" ] || die "no KERNEL_RPMS_REF — run the build-kernel CI job (or podman build -f os/kernel/Containerfile) first"
msg "1/3 podman build $QOS_IMAGE (in-image compile; SOURCE_DATE_EPOCH=$SOURCE_DATE_EPOCH; rpms=$RPMS_REF; kernel=$KERNEL_RPMS_REF)"
podman build -f "$QOS_OS_DIR/Containerfile" -t "$QOS_IMAGE" \
  --build-arg "SOURCE_DATE_EPOCH=$SOURCE_DATE_EPOCH" \
  --build-arg "RPMS_REF=$RPMS_REF" \
  --build-arg "KERNEL_RPMS_REF=$KERNEL_RPMS_REF" \
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
