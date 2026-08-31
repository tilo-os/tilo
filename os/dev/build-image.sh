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
#   2. os/dev/disk-image.sh -> os/output/qcow2/disk.qcow2: systemd-repart for the
#      partitions, `bootc install to-filesystem` into them. That replaced
#      bootc-image-builder, which has no composefs/UKI surface — and which cannot
#      build this image at all, because qOS asks for systemd-boot and bib's
#      ostree-backend install refuses it ("bootupd is required for ostree-based
#      installs").
#
# Notes / gotchas baked in from bringup:
#   * First build is a long COLD Rust compile inside the container; the
#     Containerfile uses cargo + sccache cache mounts so rebuilds are fast.
#   * QOS_WITH_BROWSER=0 ./os/dev/build-image.sh omits the browser and the WPE
#     WebKit engine stage that builds its engine from source — ~45 of the ~60
#     minutes a cold build costs. The boot chain (kernel, initramfs, LUKS/TPM,
#     greeter, installer, rollback) is all still there, so this is the image to
#     build when the question is "does it boot?". Default is the full image.
#   * The image is SEALED: it carries a Unified Kernel Image whose signed command
#     line includes the hardening kargs and `qos.generation=<N>`, which
#     systemd-stub measures into PCR 11. Two consequences for this script:
#       - the generation has to come from somewhere real, so it is resolved from
#         the published series (os/dev/next-generation.sh) rather than defaulted;
#       - the build runs TWICE when bootc#2334 bites, because `bootc container
#         ukify` and `bootc install` compute different composefs digests for the
#         same image. The loop below reads the digest install asked for, rebuilds
#         the UKI sealing it, and installs again. Only the UKI stage rebuilds — the
#         rootfs digest is unchanged, which is why two passes converge.
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
# Rust resolve pinned (the tracked Cargo.lock), two builds of the same commit
# yield the same image (modulo dnf package drift over time — see
# docs/supply-chain.md). Override via $SOURCE_DATE_EPOCH.
SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git -C "$QOS_REPO" log -1 --format=%ct 2>/dev/null || echo 0)}"
export SOURCE_DATE_EPOCH
# The digest-pinned base the build starts FROM — our own mirror of the Fedora
# bootc image, because upstream deletes the manifest a digest points at within
# about a day (see os/base-snapshot.env). Written by the mirror-base CI job.
# shellcheck disable=SC1091
[ -f "$QOS_OS_DIR/base-snapshot.env" ] && . "$QOS_OS_DIR/base-snapshot.env"
[ -n "${BASE_REF:-}" ] || die "no BASE_REF — run the mirror-base CI job first"
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
# Pre-flight: every host-side COPY source exists. Decidable in a second, and the
# alternative is finding out ~20 minutes in from a message naming a path with a
# leading slash that exists nowhere — which is how a stale `COPY docs/secure-boot.md`
# blocked every image build on main until someone tried to build one.
# QOS_SKIP_BUILD=1 images an image that is already in local storage instead of
# building one. The image builds in CI on this project, so the local half of
# this pipeline is usually the disk step alone:
#
#   podman pull code.podesta.ai/qos/qos:<tag>
#   podman tag  code.podesta.ai/qos/qos:<tag> localhost/qos:latest
#   QOS_SKIP_BUILD=1 ./os/dev/build-image.sh
#
# The seal loop further down still runs, and still has to: it rewrites the UKI
# *inside* the image, which is a layer on top of whatever was pulled rather than
# a rebuild of it, so it works the same on an image this machine never compiled.
: "${QOS_SKIP_BUILD:=0}"

# Both of these check what a build would produce, so they have nothing to say
# about an image that already exists.
if [ "$QOS_SKIP_BUILD" != "1" ]; then
bash "$QOS_OS_DIR/tests/containerfile_sources.sh" >/dev/null \
  || { bash "$QOS_OS_DIR/tests/containerfile_sources.sh"; die "a Containerfile COPY source is missing (see above)"; }
# Pre-flight: the preinstalled app store can be built and will resolve at run
# time. Same reasoning — a runtime version the apps don't declare, or an app
# directory cargo can't name, costs a full build to discover and then shows up
# as an empty launcher rather than as a build error.
bash "$QOS_OS_DIR/tests/app_store.sh" >/dev/null \
  || { bash "$QOS_OS_DIR/tests/app_store.sh"; die "the app store would not work (see above)"; }
fi

# Where the build unpacks layers. Two failure modes converge here, and neither
# names its cause:
#
#   * /tmp is a 16 GiB tmpfs on the reference machine. A build that fills it fails
#     as "Quota exceeded" from whichever command happened to be writing, which is
#     never the one at fault.
#   * a temp dir *inside the repository* lands in the build context, so the
#     builder stage's `COPY . /build` copies the previous build's scratch into this
#     one — gigabytes of it, differing per run.
#
# os/output is on the big disk and already excluded from the context, so it is both.
: "${TMPDIR:=$QOS_OUTPUT/.tmp}"
mkdir -p "$TMPDIR"
export TMPDIR

# The generation number the image will carry and seal. Resolved from the published
# series unless given, and never defaulted: a constant here is what made every
# image claim to be generation 0, and a rollback floor that cannot move is a
# machine that will not unseal after a few updates. Reproducibility is
# "same commit + same QOS_GENERATION -> same image" — a published image is genuinely
# new content, so it must be a new generation.
if [ -z "${QOS_GENERATION:-}" ] && [ "$QOS_SKIP_BUILD" = "1" ]; then
  # Read back, not allocated: the number is already sealed into this image's
  # UKI, and allocating a second would consume a generation that no published
  # image carries -- which is the series the rollback floor counts along.
  QOS_GENERATION="$(podman run --rm --entrypoint cat "$QOS_IMAGE" /usr/lib/qos/generation 2>/dev/null | tr -d '[:space:]')"
  [ -n "$QOS_GENERATION" ] || QOS_GENERATION="unknown"
  msg "generation $QOS_GENERATION (read from $QOS_IMAGE, not allocated)"
fi
if [ -z "${QOS_GENERATION:-}" ]; then
  QOS_GENERATION="$("$QOS_OS_DIR/dev/next-generation.sh")" \
    || die "could not resolve the generation for this image (see above).
  Pass one explicitly if you know the series: QOS_GENERATION=<N> $0"
fi
export QOS_GENERATION
# The negative control, off by default: QOS_SEAL_GENERATION=0 builds a UKI that
# measures normally and carries no generation, which is how the boot-side check is
# shown to fail on the generation assertions rather than on "nothing measured".
: "${QOS_SEAL_GENERATION:=1}"

# Browser opt-out (see the header). Exported as a build-arg either way so the
# value is explicit in the build log.
: "${QOS_WITH_BROWSER:=1}"
# Extra podman-build flags, for the times the layer cache is the problem rather
# than the source. A disk that fills mid-build can leave a cached layer holding a
# corrupt RPM database, which then surfaces as dnf insisting that packages the
# base image plainly has are missing — QOS_BUILD_ARGS=--no-cache is the way out.
read -r -a _qos_build_args <<< "${QOS_BUILD_ARGS:-}"

# $1 = the composefs digest to seal into the UKI ("" to let bootc compute it).
build_image() {
  podman build -f "$QOS_OS_DIR/Containerfile" -t "$QOS_IMAGE" \
    "${_qos_build_args[@]}" \
    --build-arg "SOURCE_DATE_EPOCH=$SOURCE_DATE_EPOCH" \
    --build-arg "BASE_REF=$BASE_REF" \
    --build-arg "RPMS_REF=$RPMS_REF" \
    --build-arg "KERNEL_RPMS_REF=$KERNEL_RPMS_REF" \
    --build-arg "QOS_WITH_BROWSER=$QOS_WITH_BROWSER" \
    --build-arg "QOS_GENERATION=$QOS_GENERATION" \
    --build-arg "QOS_SEAL_GENERATION=$QOS_SEAL_GENERATION" \
    --build-arg "QOS_CFS_DIGEST=${1:-}" \
    --source-date-epoch "$SOURCE_DATE_EPOCH" --rewrite-timestamp \
    "$QOS_REPO"
}

# Rewrite the composefs digest inside the UKI the image already carries, as a layer
# that touches nothing but /boot. $1 = the digest (no "sha512:" prefix).
reseal_uki() {
  # The script is bind-mounted rather than COPYed because the layer this adds
  # must touch nothing outside /boot: /boot is the one tree excluded from the
  # composefs digest, and a file landing anywhere else would move the digest
  # this pass exists to converge on.
  #
  # It is staged into a scratch dir first. Under SELinux a bind mount of a file
  # labelled user_home_t is unreadable from container_t, and the kernel's denial
  # arrives as a bare "python3: can't open file ...: Permission denied" naming
  # neither SELinux nor the label. `z` has podman relabel the source, so it is
  # pointed at a copy: relabelling the tracked file would leave the repository
  # carrying a container label as a side effect of imaging.
  local stage rc
  stage="$(mktemp -d "${TMPDIR:-/tmp}/qos-reseal.XXXXXX")" || return 1
  cp "$QOS_OS_DIR/dev/uki-cmdline.py" "$stage/uki-cmdline.py" || { rm -rf "$stage"; return 1; }
  podman build -t "$QOS_IMAGE" -f - "$stage" <<EOF
FROM $QOS_IMAGE
RUN --mount=type=bind,source=uki-cmdline.py,target=/run/uki-cmdline.py,z \\
    python3 /run/uki-cmdline.py set "composefs=$1" \\
 && python3 /run/uki-cmdline.py show
EOF
  rc=$?
  rm -rf "$stage"
  return $rc
}

if [ "$QOS_SKIP_BUILD" = "1" ]; then
  podman image exists "$QOS_IMAGE" || die "QOS_SKIP_BUILD=1, but $QOS_IMAGE is not in local
  storage, so there is nothing to image. Pull the published image and tag it:
    podman pull code.podesta.ai/qos/qos:<tag>
    podman tag  code.podesta.ai/qos/qos:<tag> $QOS_IMAGE"
  msg "1/2 skipped (QOS_SKIP_BUILD=1): imaging the existing $QOS_IMAGE"
else
  msg "1/2 podman build $QOS_IMAGE (in-image compile; SOURCE_DATE_EPOCH=$SOURCE_DATE_EPOCH; base=$BASE_REF; rpms=$RPMS_REF; kernel=$KERNEL_RPMS_REF; browser=$QOS_WITH_BROWSER; generation=$QOS_GENERATION)"
  build_image ""
fi

msg "2/2 disk image -> qcow2"
# shellcheck disable=SC1091
. "$QOS_OS_DIR/account.env"
rm -rf "$QOS_OUTPUT/qcow2" "$QOS_OUTPUT"/manifest* 2>/dev/null || true
OUTDIR="$(dirname "$QOS_DISK")"
INSTALL_LOG="$OUTDIR/.install.log"
disk_args=(--user "$QOS_ACCOUNT")
if [ -n "${QOS_TARGET_IMGREF:-}" ]; then
  disk_args+=(--target-imgref "$QOS_TARGET_IMGREF")
fi

# Pass 1. A sealed image is expected to be refused here: `bootc container ukify`
# and `bootc install` compute different composefs digests for the same image
# (bootc#2334), and install will not accept a UKI sealing anything but its own
# value. disk-image.sh recognises that failure and prints the digest to seal, so
# the recovery is to rebuild the UKI with it and install again.
if ! "$QOS_OS_DIR/dev/disk-image.sh" "$QOS_IMAGE" "$OUTDIR" "${disk_args[@]}"; then
  want="$(sed -n "s/.*should be sha512:\([0-9a-f]\{128\}\).*/\1/p" "$INSTALL_LOG" 2>/dev/null | tail -1)"
  [ -n "$want" ] || die "the disk-image step failed for a reason other than the composefs digest
  seal; see the message above and $INSTALL_LOG"
  had="$(sed -n "s/.*is 'sha512:\([0-9a-f]\{128\}\)'.*/\1/p" "$INSTALL_LOG" 2>/dev/null | tail -1)"
  msg "bootc#2334: the UKI sealed sha512:${had:0:32}… but install wants sha512:${want:0:32}…"
  msg "pass 2: rewriting that digest into the UKI the image already carries"
  # NOT a rebuild. Rebuilding the UKI does not converge: it produces a new rootfs
  # layer, so install then computes a third digest, and a fourth — measured here as
  # 0196…, then 40b1…, then 317b… . Rewriting the digest inside the existing UKI
  # cannot move the target, because the added layer touches only /boot and /boot is
  # the one tree excluded from the composefs digest. So this converges in exactly two
  # passes, and it does not depend on the layer cache holding the rootfs identical
  # between them.
  #
  # Both digests are 128 hex characters, so the edit is length-preserving and the PE
  # layout is untouched; uki-cmdline.py refuses anything else.
  reseal_uki "$want" || die "could not rewrite the sealed composefs digest"
  if ! "$QOS_OS_DIR/dev/disk-image.sh" "$QOS_IMAGE" "$OUTDIR" "${disk_args[@]}"; then
    want2="$(sed -n "s/.*should be sha512:\([0-9a-f]\{128\}\).*/\1/p" "$INSTALL_LOG" 2>/dev/null | tail -1)"
    if [ -n "$want2" ] && [ "$want2" != "$want" ]; then
      die "the digest install expects MOVED between passes (${want:0:16}… -> ${want2:0:16}…),
  even though the only thing that changed was the UKI under /boot. That means the
  digest is not computed over the rootfs alone, so sealing it is circular by
  construction and cannot be closed here — it needs the upstream fix for bootc#2334."
    fi
    die "the second install pass failed too; see above and $INSTALL_LOG"
  fi
  msg "seal closed in two passes (install's expected digest was stable)"
fi

msg "DONE: $QOS_DISK (generation $QOS_GENERATION)"
ls -lh "$QOS_DISK"
