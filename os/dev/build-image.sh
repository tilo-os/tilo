#!/bin/bash
# Build the Tilo bootc image and a bootable qcow2, end to end.
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
#      build this image at all, because Tilo asks for systemd-boot and bib's
#      ostree-backend install refuses it ("bootupd is required for ostree-based
#      installs").
#
# Build notes:
#   * First build is a long COLD Rust compile inside the container; the
#     Containerfile uses cargo + sccache cache mounts so rebuilds are fast.
#   * TILO_WITH_BROWSER=0 ./os/dev/build-image.sh omits the Browser and its
#     Chromium engine runtime. The boot chain (kernel, initramfs, LUKS/TPM,
#     greeter, installer, rollback) is all still there, so this is the image to
#     build when the question is "does it boot?". Default is the full image.
#   * TILO_IMAGE_ONLY=1 builds the container and stops: the base a dev image is
#     derived from (os/dev/build-dev-image.sh, tilo-vm vm upgrade --from local).
#   * The image is SEALED: it carries a Unified Kernel Image whose signed command
#     line includes the hardening kargs and `tilo.generation=<N>`, which
#     systemd-stub measures into PCR 11. Two consequences for this script:
#       - the generation is resolved from the published series
#         (os/dev/next-generation.sh) rather than defaulted;
#       - the build runs TWICE when bootc#2334 bites, because `bootc container
#         ukify` and `bootc install` compute different composefs digests for the
#         same image. The loop below reads the digest install asked for, rebuilds
#         the UKI sealing it, and installs again. Only the UKI stage rebuilds — the
#         rootfs digest is unchanged, which is why two passes converge.
source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"
warn_slow_podman_storage

# Fail loudly: a broken container build must NOT silently fall through to imaging
# a stale container (which previously produced a misleadingly "successful" qcow2
# built from an old image). pipefail also catches `podman save | podman load`.
set -euo pipefail

for a in "$@"; do
  case "$a" in
    -h|--help) sed -n '2,39p' "$0"; exit 0 ;;
    *) die "unknown arg: $a (host cargo build is gone; binaries compile in-image)" ;;
  esac
done

# Development trust currently stages a container only: registry provisioning and
# signed disk/VM integration are separate required work.
case "${TILO_RAW_ROOTFS_ONLY:-0}" in
  0) ;;
  1)
    [ "${TILO_PUBLIC_TRUST_MODE:-production}" = development ] \
      || die "raw-only builds are development-only"
    [ "${TILO_SKIP_BUILD:-0}" = 0 ] || die "raw-only builds require a fresh source build"
    [ -n "${TILO_RAW_ROOTFS_RECEIPT:-}" ] || die "raw-only builds require TILO_RAW_ROOTFS_RECEIPT"
    ;;
  *) die "TILO_RAW_ROOTFS_ONLY must be 0 or 1" ;;
esac
_tilo_profile_args=()
_tilo_podman_args=()
_tilo_trust_args=(--build-arg TILO_PUBLIC_TRUST_MODE=production --build-arg TILO_PUBLIC_TRUST_MANIFEST_SHA256=)
case "${TILO_PUBLIC_TRUST_MODE:-production}" in
  production)
    [ -z "${TILO_RAW_ROOTFS_RECEIPT:-}" ] || die "raw rootfs handoff is development-only"
    [ -z "${TILO_PUBLIC_TRUST_BUNDLE:-}${TILO_PUBLIC_TRUST_MANIFEST_SHA256:-}" ] \
      || die "development bundle inputs require TILO_PUBLIC_TRUST_MODE=development"
    ;;
  development)
    [ "${TILO_SKIP_BUILD:-0}" != 1 ] || die "development trust requires a fresh build"
    [ "$(id -u)" -eq 0 ] || die "development signing requires a rootful Linux build and Podman store"
    [ -n "${TILO_PCR_PREDICTION_SIGNER:-}" ] || die "development builds require the independent EFI/PCR signer"
    [ -z "${CONTAINER_HOST:-}${CONTAINER_CONNECTION:-}${CONTAINER_SSHKEY:-}${DOCKER_HOST:-}" ] || die "development signing requires local Podman storage"
    _tilo_podman_args=(--remote=false)
    _tilo_development_revision=$(git -C "$TILO_REPO" rev-parse --verify HEAD) \
      || die "development builds require a committed source revision"
    [ -z "$(git -C "$TILO_REPO" status --porcelain --untracked-files=all)" ] \
      || die "commit development source changes before building a signed candidate"
    [[ "$TILO_IMAGE" =~ ^localhost/tilo-development:[a-zA-Z0-9_][a-zA-Z0-9_.-]*$ ]] \
      || die "development trust requires TILO_IMAGE=localhost/tilo-development:<tag>"
    [ -n "${TILO_PUBLIC_TRUST_BUNDLE:-}" ] && [ -n "${TILO_PUBLIC_TRUST_MANIFEST_SHA256:-}" ] \
      || die "development trust requires a public bundle and independently pinned manifest digest"
    python3 "$TILO_OS_DIR/dev/hardware_profile.py" --development-target "${TILO_TARGET_IMGREF:-}" >/dev/null \
      || die "development trust requires an explicit isolated development registry origin"
    _tilo_profile_args=(--development-trust)
    _tilo_trust_result=$(python3 "$TILO_OS_DIR/dev/development-trust.py" validate \
      "$TILO_PUBLIC_TRUST_BUNDLE" --expected-manifest-sha256 "$TILO_PUBLIC_TRUST_MANIFEST_SHA256") \
      || die "invalid development public trust bundle"
    _tilo_dev_owner=$(python3 -c 'import json,sys; print(json.load(sys.stdin)["identities"]["registry-owner.pub"])' <<< "$_tilo_trust_result")
    [ -z "${TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY:-}" ] || [ "$TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY" = "$_tilo_dev_owner" ] \
      || die "registry owner differs from development public bundle"
    TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY=$_tilo_dev_owner
      _tilo_trust_args=(--build-context "public-trust=$(realpath "$TILO_PUBLIC_TRUST_BUNDLE")"
        --label "org.opencontainers.image.revision=$_tilo_development_revision"
      --build-arg TILO_PUBLIC_TRUST_MODE=development
      --build-arg "TILO_PUBLIC_TRUST_MANIFEST_SHA256=$TILO_PUBLIC_TRUST_MANIFEST_SHA256")
    ;;
  *) die "TILO_PUBLIC_TRUST_MODE must be production or development" ;;
esac

# The kernel is replaceable for local work: TILO_KERNEL_TRUST=unsigned accepts any
# KERNEL_RPMS_REF (a CI-built kernel by digest, or a local
# `podman build -f os/kernel/Containerfile -t localhost/tilo-kernel:dev`) and skips
# only its signature and provenance checks. Never in CI, never for the published
# production image; the resulting image is labelled so it cannot pass for signed.
# TILO_RPMS_TRUST=unsigned does the same for the frozen RPM snapshot: a local
# `os/dev/build-local-rpms.sh` snapshot lets a package-set change be built before
# CI has published and signed its snapshot. The snapshot must still match the
# Containerfiles' package set; only its signature, provenance and two-architecture
# index are not checked.
for _tilo_input_trust in "${TILO_KERNEL_TRUST:=signed}" "${TILO_RPMS_TRUST:=signed}"; do
  if [ "$_tilo_input_trust" = unsigned ]; then
    [ -z "${GITHUB_ACTIONS:-}${FORGEJO_ACTIONS:-}${CI:-}" ] \
      || die "unsigned kernel or RPM inputs are for local builds; CI publishes only signed ones"
    case "${TILO_TARGET_IMGREF:-}" in
      registry.tilo.so/tilo/tilo:*|registry.tilo.so/tilo/tilo@*)
        die "unsigned kernel or RPM inputs cannot target the production image $TILO_TARGET_IMGREF" ;;
    esac
  fi
done
case "$TILO_RPMS_TRUST" in signed|unsigned) ;; *) die "TILO_RPMS_TRUST must be signed or unsigned" ;; esac
case "${TILO_KERNEL_TRUST:=signed}" in
  signed) ;;
  unsigned) ;;
  *) die "TILO_KERNEL_TRUST must be signed or unsigned" ;;
esac

# Hardware profiles affect the sealed rootfs and UKI. Generic builds remain the
# default; a machine-specific image must be requested explicitly.
: "${TILO_HARDWARE_PROFILE:=generic}"
if [ "$TILO_HARDWARE_PROFILE" != generic ]; then
    [ "${TILO_SKIP_BUILD:-0}" != 1 ] \
      || die "TILO_SKIP_BUILD cannot apply a hardware profile; rebuild the image with its firmware and device tree"
    python3 "$TILO_OS_DIR/dev/hardware_profile.py" "$TILO_HARDWARE_PROFILE" \
      "$TILO_OS_DIR/hardware/profiles" "${TILO_BUILD_ARCH:-$(uname -m)}" "$TILO_OS_DIR/hardware/firmware" \
      "${TILO_TARGET_IMGREF:-}" "${_tilo_profile_args[@]}" \
      || die "hardware-profile build inputs are invalid; see os/hardware/README.md"
fi

# Reproducible builds: derive a deterministic SOURCE_DATE_EPOCH from the HEAD
# commit time (same commit -> same value), pass it through to the build, and
# normally clamp layer + image timestamps to it. Verified raw-only assembly may
# explicitly use TILO_RAW_LAYER_EPOCH for OCI normalization; source inputs and
# signing retain SOURCE_DATE_EPOCH. With the base pinned by digest and the
# Rust resolve pinned (the tracked Cargo.lock), two builds of the same commit
# yield the same image (modulo dnf package drift over time — see
# docs/supply-chain.md). Override via $SOURCE_DATE_EPOCH.
if [ -z "${SOURCE_DATE_EPOCH:-}" ]; then
  SOURCE_DATE_EPOCH="$(git -C "$TILO_REPO" log -1 --format=%ct 2>/dev/null || true)"
fi
# A zero clock is what a non-git mirror produces. DNF then rejects every RPM
# header. A cargo drop-in sets this from the real checkout; refuse the fallback.
case "$SOURCE_DATE_EPOCH" in
  ''|0|*[!0-9]*)
    die "SOURCE_DATE_EPOCH must be a positive commit time. A zero clock makes DNF reject RPM headers. Set SOURCE_DATE_EPOCH=\$(git log -1 --format=%ct) from the real checkout."
    ;;
esac
export SOURCE_DATE_EPOCH
# Resolve Windows providers before downloading assets or starting compilation.
# No ARM payload pins are fabricated: supply published, validated digest refs.
TILO_WINDOWS_ARCH="${TILO_BUILD_ARCH:-$(uname -m)}"
case "${TILO_NATIVE_BUILDER_ONLY:-0}" in
  0) ;;
  1)
    [ "${TILO_PUBLIC_TRUST_MODE:-production}" = development ] \
      && [ "${TILO_SKIP_BUILD:-0}" = 0 ] \
      && [ "$TILO_HARDWARE_PROFILE" = generic ] \
      || die "native payload mode requires a fresh generic development build"
    ;;
  *) die "TILO_NATIVE_BUILDER_ONLY must be 0 or 1" ;;
esac
# The pins alone decide whether the image includes the provider (CI derives it
# with the same command): x86_64 always does, ARM once both refs are set and
# not before. TILO_WITH_WINDOWS is therefore derived, never taken from the caller.
TILO_WITH_WINDOWS=1
if [ "${TILO_SKIP_BUILD:-0}" != 1 ] && [ "${TILO_NATIVE_BUILDER_ONLY:-0}" = 0 ]; then
  TILO_WITH_WINDOWS=$(python3 "$TILO_OS_DIR/dev/windows_provider.py" mode --arch "$TILO_WINDOWS_ARCH" \
    --fex-ref "${FEX_PAYLOAD_REF:-}" --guest-ref "${WINDOWS_GUEST_REF:-}") \
    || die "Windows provider inputs are incomplete; see os/docs/windows-provider-packaging.md"
  [ "$TILO_WITH_WINDOWS" = 1 ] \
    || warn "no FEX_PAYLOAD_REF/WINDOWS_GUEST_REF: this $TILO_WINDOWS_ARCH image is built without Windows apps"
fi
# Disk-only installation uses an image with its external assets already baked
# in, so it does not need the build-context downloads.
if [ "${TILO_SKIP_BUILD:-0}" != 1 ] && [ "${TILO_NATIVE_BUILDER_ONLY:-0}" = 1 ]; then
  _tilo_native_assets=()
  if [ "${TILO_WITH_BROWSER:-1}" = 1 ]; then
    case "$TILO_WINDOWS_ARCH" in
      x86_64) _tilo_cef_platform=linux64 ;;
      aarch64) _tilo_cef_platform=linuxarm64 ;;
      *) die "unsupported browser engine architecture" ;;
    esac
    _tilo_cef_version=$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["chromium_version"])' \
      "$TILO_REPO/scripts/browser/cef-lock.json") || die "could not read the pinned CEF version"
    _tilo_native_assets+=("cef-${_tilo_cef_version}-${_tilo_cef_platform}-minimal.tar.bz2")
  fi
  _tilo_sccache_version=$(sed -nE 's/^ARG SCCACHE_VERSION=([0-9]+\.[0-9]+\.[0-9]+)$/\1/p' "$TILO_OS_DIR/Containerfile")
  [[ "$_tilo_sccache_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "expected one pinned sccache version"
  case "$TILO_WINDOWS_ARCH" in
    aarch64|x86_64) ;;
    *) die "unsupported native sccache architecture" ;;
  esac
  _tilo_native_assets+=("sccache-v${_tilo_sccache_version}-${TILO_WINDOWS_ARCH}-unknown-linux-musl.tar.gz")
  _tilo_mold_version=$(sed -nE 's/^ARG MOLD_VERSION=([0-9]+\.[0-9]+\.[0-9]+)$/\1/p' "$TILO_OS_DIR/Containerfile")
  [[ "$_tilo_mold_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "expected one pinned mold version"
  _tilo_native_assets+=("mold-${_tilo_mold_version}-${TILO_WINDOWS_ARCH}-linux.tar.gz")
  "$TILO_OS_DIR/dev/cache-github-assets.sh" "${_tilo_native_assets[@]}"
elif [ "${TILO_SKIP_BUILD:-0}" != 1 ]; then
  "$TILO_OS_DIR/dev/cache-github-assets.sh"
fi
# The digest-pinned base the build starts FROM — the mirror of the Fedora
# bootc image, because upstream deletes the manifest a digest points at within
# about a day (see os/base-snapshot.env). Written by the mirror-base CI job.
# shellcheck disable=SC1091
[ -f "$TILO_OS_DIR/base-snapshot.env" ] && . "$TILO_OS_DIR/base-snapshot.env"
[ -n "${BASE_REF:-}" ] || die "no BASE_REF — run the mirror-base CI job first"
# The digest-pinned frozen RPM repo the build installs from (written by
# the snapshot-rpms CI job). Cross-time reproducible.
# shellcheck disable=SC1091
# An explicit RPMS_REF is useful for validating a just-built snapshot before it
# is published and pinned. Do not let the tracked default silently overwrite it.
_tilo_rpms_ref_override="${RPMS_REF:-}"
if [ -n "$_tilo_rpms_ref_override" ]; then
  RPMS_REF="$_tilo_rpms_ref_override"
elif [ "${TILO_SKIP_BUILD:-0}" = 1 ]; then
  # Imaging an image that already exists installs nothing from a snapshot, and
  # the one it was built from (possibly local to another machine) need not
  # resolve here.
  RPMS_REF=""
elif RPMS_REF="$("$TILO_OS_DIR/dev/resolve-rpm-snapshot.sh")"; then
  :
else
  die "no RPMS_REF — publish set-<fingerprint> via snapshot-rpms or follow os/docs/rpm-snapshot-packaging.md"
fi
[ -n "${RPMS_REF:-}" ] || [ "${TILO_SKIP_BUILD:-0}" = 1 ] \
  || die "no RPMS_REF — publish set-<fingerprint> via snapshot-rpms or follow os/docs/rpm-snapshot-packaging.md"
# The env-runtime environment image the system app store ships, resolved the
# way CI resolves it (by the fingerprint of os/Containerfile.env-runtime). An
# explicit ENV_RUNTIME_REF wins; with none published yet the image ships none.
if [ -z "${ENV_RUNTIME_REF:-}" ] && [ "${TILO_SKIP_BUILD:-0}" != 1 ]; then
  if ! ENV_RUNTIME_REF="$("$TILO_OS_DIR/dev/resolve-env-runtime.sh")"; then
    "$TILO_OS_DIR/dev/resolve-env-runtime.sh" --exists \
      && die "the published env-runtime does not verify"
    warn "no env-runtime published for os/Containerfile.env-runtime: this image ships none"
    ENV_RUNTIME_REF=scratch
  fi
fi
# The digest-pinned tilo-kernel image (built by .forgejo/workflows/build-kernel.yml
# — see os/kernel/Containerfile).  os/Containerfile swaps kernel-core →
# tilo-kernel-core out of it.  Cross-time reproducible.
# An explicit digest is useful while validating a newly published kernel, but
# it is held to the same repository and signature policy as the tracked pin.
if [ "${TILO_NATIVE_BUILDER_ONLY:-0}" = 1 ]; then
  # This target does not depend on the kernel stage. An inert scratch stage
  # avoids requiring or fabricating an unrelated architecture-specific pin.
  KERNEL_RPMS_REF=scratch
else
_tilo_kernel_ref_override="${KERNEL_RPMS_REF:-}"
# shellcheck disable=SC1091
[ -f "$TILO_OS_DIR/kernel-snapshot.env" ] && . "$TILO_OS_DIR/kernel-snapshot.env"
case "$(uname -m)" in
  x86_64) _tilo_native_kernel_ref="${KERNEL_RPMS_X86_64_REF:-}"; _tilo_registry_arch=amd64 ;;
  aarch64) _tilo_native_kernel_ref="${KERNEL_RPMS_AARCH64_REF:-}"; _tilo_registry_arch=arm64 ;;
  *) die "unsupported kernel architecture: $(uname -m)" ;;
esac
KERNEL_RPMS_REF="${_tilo_kernel_ref_override:-$_tilo_native_kernel_ref}"
[ -n "${KERNEL_RPMS_REF:-}" ] || die "no KERNEL_RPMS_REF — run the build-kernel CI job (or podman build -f os/kernel/Containerfile) first"
fi
# Pre-flight: every host-side COPY source exists. Decidable in a second, and the
# alternative is finding out ~20 minutes in from a message naming a path with a
# leading slash that exists nowhere — which is how a stale `COPY docs/secure-boot.md`
# blocked every image build on main until someone tried to build one.
# TILO_SKIP_BUILD=1 images an image that is already in local storage instead of
# building one. The image builds in CI on this project, so the local half of
# this pipeline is usually the disk step alone:
#
#   podman pull registry.tilo.so/tilo/tilo:<tag>
#   podman tag  registry.tilo.so/tilo/tilo:<tag> localhost/tilo:latest
#   TILO_SKIP_BUILD=1 ./os/dev/build-image.sh
#
# The seal loop further down still runs, and still has to: it rewrites the UKI
# *inside* the image, which is a layer on top of whatever was pulled rather than
# a rebuild of it, so it works the same on an image this machine never compiled.
: "${TILO_SKIP_BUILD:=0}"

# Both of these check what a build would produce, so they have nothing to say
# about an image that already exists.
if [ "$TILO_SKIP_BUILD" != "1" ]; then
if [ "${TILO_PUBLIC_TRUST_MODE:-production}" = production ]; then
if [ "$TILO_RPMS_TRUST" = unsigned ]; then
  warn "TILO_RPMS_TRUST=unsigned: not verifying the signature or provenance of the RPM snapshot $RPMS_REF"
else
  [[ "$RPMS_REF" =~ ^registry\.tilo\.so/tilo/rpms@sha256:[0-9a-f]{64}$ ]] \
    || die "RPMS_REF must pin registry.tilo.so/tilo/rpms by sha256 digest"
  command -v cosign >/dev/null || die "building an OS image requires cosign to verify the frozen RPM repository"
  _tilo_rpms_digest="${RPMS_REF##*@}"
  _tilo_package_set_sha256="$({ "$TILO_OS_DIR/dev/derive-rpm-package-set.py" --arch x86_64; "$TILO_OS_DIR/dev/derive-rpm-package-set.py" --arch aarch64; } | LC_ALL=C sort -u | sha256sum | cut -d' ' -f1)"
  _tilo_native_package_set_sha256="$("$TILO_OS_DIR/dev/derive-rpm-package-set.py" --arch "$(uname -m)" | LC_ALL=C sort -u | sha256sum | cut -d' ' -f1)"
  _tilo_rpms_provenance="$(cosign verify-attestation \
    --key "$TILO_REPO/keys/cosign.pub" --type slsaprovenance1 "$RPMS_REF")" \
    || die "frozen RPM provenance verification failed"
  cosign verify --key "$TILO_REPO/keys/cosign.pub" "$RPMS_REF" >/dev/null \
    || die "frozen RPM signature verification failed"
  printf '%s' "$_tilo_rpms_provenance" | jq -e \
    --arg digest "$_tilo_rpms_digest" --arg base "$BASE_REF" \
    --arg package_set_sha256 "$_tilo_package_set_sha256" \
    --arg native_package_set_sha256 "$_tilo_native_package_set_sha256" \
    --arg native_arch "$_tilo_registry_arch" '
      [(if type == "array" then .[] else . end) | .payload | @base64d | fromjson | .predicate]
      | any(
          ((.buildDefinition.buildType == "https://tilo.sh/buildtypes/frozen-rpm-index/v1")
            or (.buildDefinition.buildType | endswith("/buildtypes/frozen-rpm-index/v1")))
          and .buildDefinition.externalParameters.architectures == ["amd64", "arm64"]
          and .buildDefinition.externalParameters.packageSetSha256 == $package_set_sha256
          and .buildDefinition.externalParameters.packageSetSha256ByArchitecture[$native_arch] == $native_package_set_sha256
          and .buildDefinition.internalParameters.expectedReleaseDigest == $digest
          and any(.buildDefinition.resolvedDependencies[]?; .uri == $base)
        )
    ' >/dev/null || die "frozen RPM provenance does not bind its package set, digest, base, and architectures"
  command -v skopeo >/dev/null || die "building an OS image requires skopeo to inspect signed build inputs"
  _tilo_rpms_platforms="$(skopeo inspect --raw "docker://$RPMS_REF" | jq -c \
    '[.manifests[].platform | {os,architecture}] | sort_by(.architecture)')" \
    || die "could not inspect the frozen RPM index"
  [ "$_tilo_rpms_platforms" = '[{"os":"linux","architecture":"amd64"},{"os":"linux","architecture":"arm64"}]' ] \
    || die "frozen RPM index does not contain exactly amd64 and arm64"
fi
if [ "$TILO_KERNEL_TRUST" = unsigned ]; then
  warn "TILO_KERNEL_TRUST=unsigned: not verifying the kernel signature or provenance of $KERNEL_RPMS_REF"
else
  [[ "$KERNEL_RPMS_REF" =~ ^registry\.tilo\.so/tilo/tilo-kernel@sha256:[0-9a-f]{64}$ ]] \
    || die "KERNEL_RPMS_REF must pin registry.tilo.so/tilo/tilo-kernel by sha256 digest"
  command -v cosign >/dev/null || die "building an OS image requires cosign to verify the Tilo kernel"
  command -v skopeo >/dev/null || die "building an OS image requires skopeo to inspect the signed Tilo kernel"
  # The kernel binds its own signed build environment; the OS RPM snapshot
  # and base are not kernel inputs.
  "$TILO_OS_DIR/dev/verify-kernel-provenance.sh" "$KERNEL_RPMS_REF" \
    || die "the Tilo kernel $KERNEL_RPMS_REF is not authenticated for this build"
fi
fi # Production provenance; development inputs have independent pinned receipts.
bash "$TILO_OS_DIR/dev/verify-rpm-snapshot-package-set.sh" podman "$RPMS_REF" \
  || die "the frozen RPM snapshot does not match the current Containerfiles"
bash "$TILO_OS_DIR/tests/containerfile_sources.sh" >/dev/null \
  || { bash "$TILO_OS_DIR/tests/containerfile_sources.sh"; die "a Containerfile COPY source is missing (see above)"; }
# Pre-flight: the preinstalled app store can be built and will resolve at run
# time. Same reasoning — a runtime version the apps don't declare, or an app
# directory cargo can't name, costs a full build to discover and then shows up
# as an empty launcher rather than as a build error.
bash "$TILO_OS_DIR/tests/app_store.sh" >/dev/null \
  || { bash "$TILO_OS_DIR/tests/app_store.sh"; die "the app store would not work (see above)"; }
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
: "${TMPDIR:=$TILO_OUTPUT/.tmp}"
mkdir -p "$TMPDIR"
export TMPDIR

# The generation number the image will carry and seal. Resolved from the published
# series unless given, and never defaulted: a constant here is what made every
# image claim to be generation 0, and a rollback floor that cannot move is a
# machine that will not unseal after a few updates. Reproducibility is
# "same commit + same TILO_GENERATION -> same image" — a published image contains
# new content, so it must be a new generation.
if [ -z "${TILO_GENERATION:-}" ] && [ "$TILO_SKIP_BUILD" = "1" ]; then
  # Read back, not allocated: the number is already sealed into this image's
  # UKI, and allocating a second would consume a generation that no published
  # image carries -- which is the series the rollback floor counts along.
  TILO_GENERATION="$(podman run --rm --entrypoint cat "$TILO_IMAGE" /usr/lib/tilo/generation 2>/dev/null | tr -d '[:space:]')"
  [ -n "$TILO_GENERATION" ] || TILO_GENERATION="unknown"
  msg "generation $TILO_GENERATION (read from $TILO_IMAGE, not allocated)"
fi
if [ -z "${TILO_GENERATION:-}" ]; then
  TILO_GENERATION="$("$TILO_OS_DIR/dev/next-generation.sh")" \
    || die "could not resolve the generation for this image (see above).
  Pass one explicitly if you know the series: TILO_GENERATION=<N> $0"
fi
export TILO_GENERATION
# The negative control, off by default: TILO_SEAL_GENERATION=0 builds a UKI that
# measures normally and carries no generation, which is how the boot-side check is
# shown to fail on the generation assertions rather than on "nothing measured".
: "${TILO_SEAL_GENERATION:=1}"

# Browser opt-out (see the header). Exported as a build-arg either way so the
# value is explicit in the build log.
: "${TILO_WITH_BROWSER:=1}"

# Rust LTO for the in-image compile (os/Containerfile, builder stage): thin for a
# publication, which is what TILO_REQUIRE_PCR_PREDICTION=1 marks here and in CI,
# and off for everything else. Off relinks a one-crate change in seconds; thin
# relinks every binary's whole dependency graph, uncacheably. The image records
# the choice in its org.tilo.rust-lto label.
if [ -z "${TILO_RUST_LTO:-}" ]; then
  if [ "${TILO_REQUIRE_PCR_PREDICTION:-0}" = 1 ]; then TILO_RUST_LTO=thin; else TILO_RUST_LTO=off; fi
fi
case "$TILO_RUST_LTO" in
  thin|off) ;;
  *) die "TILO_RUST_LTO must be thin or off, not '$TILO_RUST_LTO'" ;;
esac
[ "$TILO_RUST_LTO" = thin ] || [ "${TILO_REQUIRE_PCR_PREDICTION:-0}" != 1 ] \
  || die "a publication (TILO_REQUIRE_PCR_PREDICTION=1) is built with TILO_RUST_LTO=thin"
# Link jobs: a thin-LTO link holds a binary's whole program in memory, so the
# Containerfile's default stays conservative; without LTO a link is light, and
# the compile may use the machine: a CPU per job, as far as memory holds out at
# about 2 GiB a job (8 jobs when memory cannot be read).
if [ -z "${TILO_CARGO_BUILD_JOBS:-}" ] && [ "$TILO_RUST_LTO" = off ]; then
  TILO_CARGO_BUILD_JOBS="$(cargo_build_jobs 2)"
fi
_tilo_jobs_args=()
[ -z "${TILO_CARGO_BUILD_JOBS:-}" ] || _tilo_jobs_args=(--build-arg "TILO_CARGO_BUILD_JOBS=$TILO_CARGO_BUILD_JOBS")
# The release profile's debug info (os/Containerfile TILO_RUST_DEBUGINFO) when set.
[ -z "${TILO_RUST_DEBUGINFO:-}" ] || _tilo_jobs_args+=(--build-arg "TILO_RUST_DEBUGINFO=$TILO_RUST_DEBUGINFO")
# CI's compiler cache, read-only: TILO_IMAGE_SCCACHE_URL is its Actions cache
# endpoint (for tilo/tilo: https://code.tilo.so/ci-cache/r/tilo/tilo/) and
# TILO_IMAGE_SCCACHE_TOKEN_FILE a file holding a Forgejo token that can read the
# repository. Both reach the Rust compile as build secrets, never as layers.
if [ -n "${TILO_IMAGE_SCCACHE_URL:-}" ]; then
  [ -r "${TILO_IMAGE_SCCACHE_TOKEN_FILE:-}" ] \
    || die "TILO_IMAGE_SCCACHE_URL needs TILO_IMAGE_SCCACHE_TOKEN_FILE, a readable file with a Forgejo token"
  export TILO_IMAGE_SCCACHE_URL
  _tilo_jobs_args+=(--secret "id=actions_cache_url,env=TILO_IMAGE_SCCACHE_URL"
    --secret "id=actions_runtime_token,src=$TILO_IMAGE_SCCACHE_TOKEN_FILE")
  msg "compiler cache: $TILO_IMAGE_SCCACHE_URL"
fi
# Extra podman-build flags, for the times the layer cache is the problem rather
# than the source. A disk that fills mid-build can leave a cached layer holding a
# corrupt RPM database, which then surfaces as dnf insisting that packages the
# base image plainly has are missing — TILO_BUILD_ARGS=--no-cache is the way out.
read -r -a _tilo_build_args <<< "${TILO_BUILD_ARGS:-}"
# The Rust compile's target cache (os/Containerfile TILO_BUILD_CACHE_KEY) is
# named by what changes the artifacts cargo produces, so builds with different
# settings sharing one podman store do not rebuild each other's target
# directory. Build arguments in TILO_BUILD_ARGS override the resolved values.
declare -A _tilo_cache_inputs=(
  [arch]="$(uname -m)"
  [TILO_RUST_LTO]="$TILO_RUST_LTO"
  [TILO_RUST_DEBUGINFO]="${TILO_RUST_DEBUGINFO:-line-tables-only}"
  [TILO_WITH_BROWSER]="$TILO_WITH_BROWSER"
  [TILO_PREBUILT_APPS]=0
  [TILO_PREBUILT_DESKTOP]=0
  [TILO_CARGO_FRESHNESS]=clean
  [TILO_COMPILER_CACHE]=shared
)
for ((_i = 0; _i < ${#_tilo_build_args[@]}; _i++)); do
  case "${_tilo_build_args[_i]}" in
    --build-arg) _kv="${_tilo_build_args[_i + 1]:-}"; _i=$((_i + 1)) ;;
    --build-arg=*) _kv="${_tilo_build_args[_i]#--build-arg=}" ;;
    *) continue ;;
  esac
  _k="${_kv%%=*}"
  [ -z "$_k" ] || [ -z "${_tilo_cache_inputs[$_k]+set}" ] || _tilo_cache_inputs[$_k]="${_kv#*=}"
done
TILO_BUILD_CACHE_KEY="$(for _k in "${!_tilo_cache_inputs[@]}"; do printf '%s=%s\n' "$_k" "${_tilo_cache_inputs[$_k]}"; done \
  | LC_ALL=C sort | sha256sum | cut -c1-12)"
TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY="${TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY:-}"
if [ "$TILO_SKIP_BUILD" != 1 ]; then
  [[ "$TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY" =~ ^[0-9a-fA-F]{64}$ ]] \
    && [ "$TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY" != 0000000000000000000000000000000000000000000000000000000000000000 ] \
    || die "set TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY to the published Tilo-main Ed25519 owner key"
fi

# $1 = the composefs digest to seal into the UKI. The first image deliberately
# carries a deterministic placeholder: bootc install computes the authoritative
# digest, and the existing /boot-only second pass replaces it without changing
# the rootfs whose digest is being sealed. Computing a provisional composefs
# repository here duplicates the rootfs and can exhaust an ephemeral builder.
TILO_PROVISIONAL_CFS_DIGEST="$(printf '0%.0s' {1..128})"
build_image() {
  local seal_digest="${1:-$TILO_PROVISIONAL_CFS_DIGEST}"
  local image_tag="${2-$TILO_IMAGE}"
  local tag_args=()
  [ -z "$image_tag" ] || tag_args=(-t "$image_tag")
  [ "$#" -eq 0 ] || shift
  [ "$#" -eq 0 ] || shift
  local layer_epoch="$SOURCE_DATE_EPOCH" build_epoch="$SOURCE_DATE_EPOCH"
  # TILO_LAYER_EPOCH=base (local builds, os/dev/local-inputs.sh): layers and the
  # build's SOURCE_DATE_EPOCH take the base image's creation time instead of the
  # commit's. podman keys every cached layer on its epoch, so the commit time
  # rebuilt the whole image after every commit; the base's time changes only
  # with the base. The commit time stays in the org.tilo.source-date-epoch label.
  case "${TILO_LAYER_EPOCH:-commit}" in
    commit) ;;
    base)
      [ "${TILO_REQUIRE_PCR_PREDICTION:-0}" != 1 ] || die "a publication dates its layers by its commit (TILO_LAYER_EPOCH=commit)"
      layer_epoch="$(tilo_image_epoch "$BASE_REF")" || die "could not read the creation time of $BASE_REF"
      build_epoch="$layer_epoch" ;;
    *) die "TILO_LAYER_EPOCH must be commit or base, not '${TILO_LAYER_EPOCH}'" ;;
  esac
  if [ -n "${TILO_RAW_LAYER_EPOCH:-}" ]; then
    # Normalize only raw assembly layers; source inputs and signing keep their epoch.
    [ "${TILO_RAW_ROOTFS_ONLY:-0}" = 1 ] &&
      [ "${TILO_PUBLIC_TRUST_MODE:-production}" = development ] &&
      [[ "${TILO_NATIVE_BUILDER_IMAGE_ID:-}" =~ ^sha256:[0-9a-f]{64}$ ]] &&
      [ "$#" = 4 ] && [ "$1" = --target ] && [ "$2" = rootfs ] &&
      [ "$3" = --iidfile ] || die "layer epoch requires verified raw-only native reuse"
    [[ "$TILO_RAW_LAYER_EPOCH" =~ ^(0|[1-9][0-9]{0,11})$ ]] &&
      (( TILO_RAW_LAYER_EPOCH <= 253402300799 )) || die "invalid raw layer epoch"
    layer_epoch="$TILO_RAW_LAYER_EPOCH"
  fi
  podman "${_tilo_podman_args[@]}" build "${TILO_PODMAN_BUILD_FLAGS[@]}" -f "$TILO_OS_DIR/Containerfile" "${tag_args[@]}" \
    --build-context "github-assets=$TILO_REPO/os/external-assets/cache" \
    --build-arg "TILO_RUST_LTO=$TILO_RUST_LTO" \
    --build-arg "TILO_BUILD_CACHE_KEY=$TILO_BUILD_CACHE_KEY" \
    "${_tilo_jobs_args[@]}" \
    "${_tilo_build_args[@]}" \
    "${_tilo_native_builder_args[@]}" \
    "${_tilo_trust_args[@]}" \
    --label "org.tilo.kernel.trust=$TILO_KERNEL_TRUST" \
    --label "org.tilo.rpms.trust=$TILO_RPMS_TRUST" \
    --label "org.tilo.rpms.ref=$RPMS_REF" \
    --label "org.tilo.source-date-epoch=$SOURCE_DATE_EPOCH" \
    --build-arg "SOURCE_DATE_EPOCH=$build_epoch" \
    --build-arg "BASE_REF=$BASE_REF" \
    --build-arg "RPMS_REF=$RPMS_REF" \
    --build-arg "KERNEL_RPMS_REF=$KERNEL_RPMS_REF" \
    --build-arg "TILO_WINDOWS_ARCH=$TILO_WINDOWS_ARCH" \
    --build-arg "FEX_PAYLOAD_REF=${FEX_PAYLOAD_REF:-scratch}" \
    --build-arg "WINDOWS_GUEST_REF=${WINDOWS_GUEST_REF:-scratch}" \
    --build-arg "ENV_RUNTIME_REF=${ENV_RUNTIME_REF:-scratch}" \
    --build-arg "TILO_WITH_WINDOWS=$TILO_WITH_WINDOWS" \
    --build-arg "TILO_WITH_BROWSER=$TILO_WITH_BROWSER" \
    --build-arg "TILO_HARDWARE_PROFILE=$TILO_HARDWARE_PROFILE" \
    --build-arg "TILO_TARGET_IMGREF=${TILO_TARGET_IMGREF:-}" \
    --build-arg "TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY=$TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY" \
    --build-arg "TILO_GENERATION=$TILO_GENERATION" \
    --build-arg "TILO_SEAL_GENERATION=$TILO_SEAL_GENERATION" \
    --build-arg "TILO_CFS_DIGEST=$seal_digest" \
    --source-date-epoch "$layer_epoch" --rewrite-timestamp \
    "$@" \
    "$TILO_REPO"
}

# Rewrite the composefs digest inside the UKI the image already carries, as a layer
# that touches nothing but /boot. $1 = the digest (no "sha512:" prefix).
reseal_uki() {
  "$TILO_OS_DIR/dev/reseal-image.sh" "$TILO_IMAGE" "$1"
}

_tilo_native_builder_args=()
if [ -n "${TILO_NATIVE_BUILDER_IMAGE_ID:-}" ]; then
  [ "${TILO_PUBLIC_TRUST_MODE:-production}" = development ] || die "native builder reuse requires development mode"
  [[ "$TILO_NATIVE_BUILDER_IMAGE_ID" =~ ^sha256:[0-9a-f]{64}$ ]] || die "native builder must be a verified immutable local image ID"
  [ "${TILO_NATIVE_BUILDER_ONLY:-0}" = 0 ] || die "cannot compile and import a native builder together"
  _tilo_native_builder_args=(--build-context "builder=container-image://$TILO_NATIVE_BUILDER_IMAGE_ID")
fi
if [ "$TILO_SKIP_BUILD" = "1" ]; then
  podman image exists "$TILO_IMAGE" || die "TILO_SKIP_BUILD=1, but $TILO_IMAGE is not in local
  storage, so there is nothing to image. Pull the published image and tag it:
    podman pull registry.tilo.so/tilo/tilo:<tag>
    podman tag  registry.tilo.so/tilo/tilo:<tag> $TILO_IMAGE"
  msg "1/2 skipped (TILO_SKIP_BUILD=1): imaging the existing $TILO_IMAGE"
else
  msg "1/2 podman build $TILO_IMAGE (in-image compile; SOURCE_DATE_EPOCH=$SOURCE_DATE_EPOCH; base=$BASE_REF; rpms=$RPMS_REF; kernel=$KERNEL_RPMS_REF; browser=$TILO_WITH_BROWSER; windows=$TILO_WITH_WINDOWS; rust-lto=$TILO_RUST_LTO; cargo-target-cache=$TILO_BUILD_CACHE_KEY; generation=$TILO_GENERATION)"
  if [ "${TILO_NATIVE_BUILDER_ONLY:-0}" = 1 ]; then
    [ "${TILO_PUBLIC_TRUST_MODE:-production}" = development ] || die "native payload compilation requires development mode"
    [ -n "${TILO_NATIVE_BUILDER_IIDFILE:-}" ] || die "native payload compilation requires an output iidfile"
    [ ! -e "$TILO_NATIVE_BUILDER_IIDFILE" ] && [ ! -L "$TILO_NATIVE_BUILDER_IIDFILE" ] || die "native builder iidfile already exists"
    build_image "$TILO_PROVISIONAL_CFS_DIGEST" "" --target native-builder-payload \
      --skip-unused-stages=true --iidfile "$TILO_NATIVE_BUILDER_IIDFILE"
    msg "Native application payload compiled; no rootfs, UKI or disk assembled."
    exit 0
  fi
  if [ "${TILO_PUBLIC_TRUST_MODE:-production}" = development ]; then
    # The signed loader changes /usr, so install it into an immutable rootfs
    # intermediate BEFORE the existing split/UKI sealing stages run.
    _tilo_signing_work=$(mktemp -d "${TILO_INSTALLER_SCRATCH:-/var/tmp}/tilo-build-signing.XXXXXX")
    trap 'rm -rf -- "$_tilo_signing_work"' EXIT
    build_image "$TILO_PROVISIONAL_CFS_DIGEST" "" --target rootfs --iidfile "$_tilo_signing_work/raw.iid"
    _tilo_raw_rootfs=$(cat "$_tilo_signing_work/raw.iid")
    [[ "$_tilo_raw_rootfs" =~ ^sha256:[0-9a-f]{64}$ ]] || die "rootfs build did not return an immutable image ID"
    if [ -n "${TILO_RAW_ROOTFS_RECEIPT:-}" ]; then
      python3 "$TILO_OS_DIR/dev/raw-rootfs-receipt.py" \
        --output "$TILO_RAW_ROOTFS_RECEIPT" --raw-rootfs-id "$_tilo_raw_rootfs" \
        --revision "$_tilo_development_revision" --source-date-epoch "$SOURCE_DATE_EPOCH" \
        --architecture "$TILO_WINDOWS_ARCH" --generation "$TILO_GENERATION" \
        --target-update-ref "$TILO_TARGET_IMGREF" \
        --public-manifest-sha256 "$TILO_PUBLIC_TRUST_MANIFEST_SHA256"
    fi
    if [ "${TILO_RAW_ROOTFS_ONLY:-0}" = 1 ]; then
      msg "Development raw rootfs staged: $_tilo_raw_rootfs. Unsealed and not installable; retention and completion require independent validation."
      exit 0
    fi
    python3 "$TILO_OS_DIR/dev/sign-development-rootfs.py" \
      --rootfs-id "$_tilo_raw_rootfs" --architecture "$TILO_WINDOWS_ARCH" \
      --revision "$_tilo_development_revision" --public-bundle "$TILO_PUBLIC_TRUST_BUNDLE" \
      --source-date-epoch "$SOURCE_DATE_EPOCH" \
      --manifest-sha256 "$TILO_PUBLIC_TRUST_MANIFEST_SHA256" \
      --signer "$TILO_PCR_PREDICTION_SIGNER" --target "$TILO_TARGET_IMGREF" \
      > "$_tilo_signing_work/receipt.json"
    _tilo_signed_rootfs=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["signed_rootfs_id"])' "$_tilo_signing_work/receipt.json")
    [[ "$_tilo_signed_rootfs" =~ ^sha256:[0-9a-f]{64}$ ]] || die "signer did not return an immutable rootfs image ID"
    build_image "$TILO_PROVISIONAL_CFS_DIGEST" "$TILO_IMAGE" --target final --pull=never --skip-unused-stages=true \
      --build-context "rootfs=container-image://$_tilo_signed_rootfs"
  else
    build_image "$TILO_PROVISIONAL_CFS_DIGEST"
  fi
fi

# TILO_IMAGE_ONLY=1 stops at the container. Its UKI still seals the placeholder
# composefs digest, so it is no install source; it is the base
# os/dev/build-dev-image.sh derives the dev image from, which unseals it anyway.
if [ "${TILO_IMAGE_ONLY:-0}" = 1 ]; then
  [ "$TILO_SKIP_BUILD" != 1 ] || die "TILO_IMAGE_ONLY=1 with TILO_SKIP_BUILD=1 does nothing"
  msg "DONE: $TILO_IMAGE (TILO_IMAGE_ONLY=1: container only, no disk and no closed seal)"
  exit 0
fi

if [ "${TILO_PUBLIC_TRUST_MODE:-production}" = development ]; then
  msg "Development container staged: $TILO_IMAGE. Disk/VM preparation remains incomplete until the isolated signed registry and disk/VM path are prepared."
  exit 0
fi

msg "2/2 disk image -> qcow2"
# shellcheck disable=SC1091
. "$TILO_OS_DIR/account.env"
rm -rf "$TILO_OUTPUT/qcow2" "$TILO_OUTPUT"/manifest* 2>/dev/null || true
OUTDIR="$(dirname "$TILO_DISK")"
INSTALL_LOG="$OUTDIR/.install.log"
disk_args=(--user "$TILO_ACCOUNT")
if [ -n "${TILO_DISK_ROOTFS:-}" ]; then
  case "$TILO_DISK_ROOTFS" in
    btrfs|ext4) disk_args+=(--rootfs "$TILO_DISK_ROOTFS") ;;
    *) die "TILO_DISK_ROOTFS must be btrfs or ext4" ;;
  esac
fi
if [ -n "${TILO_DEV_DIRECT_BUNDLE_DIR:-}" ]; then
  disk_args+=(--direct-boot-bundle "$TILO_DEV_DIRECT_BUNDLE_DIR")
fi
if [ -n "${TILO_TARGET_IMGREF:-}" ]; then
  disk_args+=(--target-imgref "$TILO_TARGET_IMGREF")
fi
# Release gate only: the /etc half of os/dev/validation-access.sh, written into
# this disk copy; `vm.sh boot` attaches the signed extension it trusts
# (TILO_VM_VALIDATION_SYSEXT).
if [ -n "${TILO_VALIDATION_ETC:-}" ]; then
  disk_args+=(--etc-overlay "$TILO_VALIDATION_ETC")
fi
# Whole-disk virtual size (disk-image.sh --size). The qcow2 stays sparse, so a
# dev VM disk with room for builds and slot layers costs nothing until written.
if [ -n "${TILO_DISK_SIZE:-}" ]; then
  disk_args+=(--size "$TILO_DISK_SIZE")
fi
# Rockchip boards: the mask ROM loads U-Boot from fixed LBAs in front of the
# ESP. TILO_ROCKCHIP_LOADER names the loader directory for an image built
# elsewhere (TILO_SKIP_BUILD); the loader is rebuilt only when its pinned
# sources change.
if [ "$TILO_HARDWARE_PROFILE" = orangepi-3b-v2-1 ] || [ -n "${TILO_ROCKCHIP_LOADER:-}" ]; then
  loader="${TILO_ROCKCHIP_LOADER:-$TILO_OUTPUT/orangepi-3b-loader}"
  "$TILO_OS_DIR/dev/build-orangepi-uboot.sh" "$loader"
  disk_args+=(--boot-gap 16M --rockchip-loader "$loader")
fi

# A bootc built from os/Containerfile.bootc-helper closes the seal in this one
# pass: install rewrites the UKI's stale composefs= digest to the one it computes
# and prints it, and the same rewrite is then applied to the image.
if [ -n "${TILO_BOOTC_HELPER:-}" ]; then
  disk_args+=(--bootc-helper "$TILO_BOOTC_HELPER" --reseal-uki)
fi

# Pass 1. A sealed image is expected to be refused here: `bootc container ukify`
# and `bootc install` compute different composefs digests for the same image
# (bootc#2334), and install will not accept a UKI sealing anything but its own
# value. disk-image.sh recognises that failure and prints the digest to seal, so
# the recovery is to rebuild the UKI with it and install again.
if ! "$TILO_OS_DIR/dev/disk-image.sh" "$TILO_IMAGE" "$OUTDIR" "${disk_args[@]}"; then
  want="$(composefs_expected_digest "$INSTALL_LOG")"
  [ -n "$want" ] || die "the disk-image step failed for a reason other than the composefs digest
  seal; see the message above and $INSTALL_LOG"
  had="$(composefs_sealed_digest "$INSTALL_LOG")"
  msg "bootc#2334: the UKI sealed sha512:${had:0:32}… but install wants sha512:${want:0:32}…"
  msg "pass 2: rewriting that digest into the UKI the image already carries"
  # NOT a rebuild. Rebuilding the UKI does not converge: it produces a new rootfs
  # layer, so install then computes a third digest, and a fourth — measured here as
  # 0196…, then 40b1…, then 317b… . reseal-image.sh extracts the existing UKI,
  # edits its command line, and adds it with COPY. A RUN layer copies up /etc and
  # changes its directory mtime in Podman, which moves bootc's composefs digest
  # even when the command edits only /boot. COPY leaves that metadata unchanged.
  #
  # Both digests are 128 hex characters, so the edit is length-preserving and the PE
  # layout is untouched; tilo-uki-cmdline refuses anything else.
  reseal_uki "$want" || die "could not rewrite the sealed composefs digest"
  if [ -n "${TILO_RESEALED_DIGEST_FILE:-}" ]; then
    printf '%s\n' "$want" > "$TILO_RESEALED_DIGEST_FILE"
  fi
  if ! "$TILO_OS_DIR/dev/disk-image.sh" "$TILO_IMAGE" "$OUTDIR" "${disk_args[@]}"; then
    want2="$(composefs_expected_digest "$INSTALL_LOG")"
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
resealed="$(sed -n 's/^bootc: resealed UKI composefs digest: \([0-9a-f]\{128\}\)$/\1/p' "$INSTALL_LOG" | tail -n1)"
if [ -n "$resealed" ]; then
  msg "bootc#2334: install resealed the UKI to sha512:${resealed:0:32}…; applying it to $TILO_IMAGE"
  reseal_uki "$resealed" || die "could not rewrite the sealed composefs digest"
  if [ -n "${TILO_RESEALED_DIGEST_FILE:-}" ]; then
    printf '%s\n' "$resealed" > "$TILO_RESEALED_DIGEST_FILE"
  fi
fi

# Publish the normal update image with the same authority sidecars as installer
# media. This runs only after the composefs seal converges; adding files under
# /boot cannot move that digest. A final image pass proves that invariant and
# ensures future bootc upgrades receive a prediction for their exact UKI.
require_prediction="${TILO_REQUIRE_PCR_PREDICTION:-0}"
allow_local_prediction="${TILO_ALLOW_LOCAL_PCR_SIGNING:-0}"
case "$require_prediction:$allow_local_prediction" in
  0:0|0:1|1:0|1:1) ;;
  *) die "TILO_REQUIRE_PCR_PREDICTION and TILO_ALLOW_LOCAL_PCR_SIGNING must be 0 or 1" ;;
esac
if [ "$require_prediction" = 1 ]; then
  _tilo_image_lto="$(podman image inspect --format '{{index .Labels "org.tilo.rust-lto"}}' "$TILO_IMAGE" 2>/dev/null || true)"
  [ "$_tilo_image_lto" = thin ] \
    || die "$TILO_IMAGE is labelled org.tilo.rust-lto='$_tilo_image_lto'; a publication ships thin-LTO binaries (TILO_RUST_LTO=thin)"
fi
if [ "$require_prediction" = 1 ] || [ "$allow_local_prediction" = 1 ]; then
  [ -n "${TILO_PCR_PREDICTION_SIGNER:-}" ] \
    || die "PCR prediction signing was requested but no TILO_PCR_PREDICTION_SIGNER is configured"
  "$TILO_OS_DIR/dev/sign-target-prediction.sh" "$TILO_IMAGE" "$TILO_PCR_PREDICTION_SIGNER"
  rm -rf "$TILO_OUTPUT/qcow2" "$TILO_OUTPUT"/manifest* 2>/dev/null || true
  "$TILO_OS_DIR/dev/disk-image.sh" "$TILO_IMAGE" "$OUTDIR" "${disk_args[@]}" \
    || die "adding /boot-only signed PCR prediction sidecars changed or invalidated the converged Tilo image"
fi

msg "DONE: $TILO_DISK (generation $TILO_GENERATION)"
ls -lh "$TILO_DISK"
