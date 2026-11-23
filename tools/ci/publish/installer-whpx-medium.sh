#!/usr/bin/env bash
# The offline installer medium Windows on ARM writes: an aarch64 live installer with one
# published, signed target preloaded, from that published live base.
#
#   installer-whpx-medium.sh PROFILE TARGET_IMAGE LIVE_BASE SOURCE_REVISION OUT
#
# TARGET_IMAGE is the immutable target child (registry.tilo.so/tilo/tilo@sha256:... for the
# generic Tilo OS, code.tilo.so/tilo/tilo@sha256:... for the Lenovo board), LIVE_BASE the
# published multi-architecture index it is a child of, and SOURCE_REVISION the commit both were
# built from. Both are verified before anything is built: the live base by its release signature,
# SLSA provenance and generation (os/dev/use-published-image.sh), the target by its cosign
# signature over the exact child the index names
# (os/dev/prepare-ci-installer-target.sh). Nothing here signs or publishes.
#
# OUT receives disk.qcow2, disk.qcow2.hardware.json, the media provenance the disk step
# wrote, and target.json: the verified target transport's own metadata, which names the
# repository, digest, architecture, hardware profile and update origin the installed machine will
# follow. Nothing else is packaged from a medium Windows writes directly.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"

profile=${1:?usage: installer-whpx-medium.sh PROFILE TARGET_IMAGE LIVE_BASE SOURCE_REVISION OUT}
target=${2:?}
live_base=${3:?}
source_revision=${4:?}
out=${5:?}
case $profile in generic|lenovo-83ed) ;; *) die "unsupported offline target profile: $profile" ;; esac
[[ $source_revision =~ ^[0-9a-f]{40}$ ]] || die "SOURCE_REVISION must be a full commit"
[[ $live_base =~ ^registry\.tilo\.so/tilo/tilo@sha256:[0-9a-f]{64}$ ]] || die "LIVE_BASE must be the published multi-architecture index"
case $profile in
  generic) [[ $target =~ ^registry\.tilo\.so/tilo/tilo@sha256:[0-9a-f]{64}$ ]] || die "the generic target is an ARM64 child of the published index" ;;
  lenovo-83ed) [[ $target =~ ^code\.tilo\.so/tilo/tilo@sha256:[0-9a-f]{64}$ ]] || die "the Lenovo target is the signed board child" ;;
esac

work="${TILO_WHPX_WORK:-/var/tmp/tilo-installer-whpx-medium}"
rm -rf "$work" "$out"
mkdir -p "$work" "$out"
trap 'rm -rf "$work"' EXIT

export RUNNER_TEMP="$work" GITHUB_PATH="$work/github-path"
export TILO_GITHUB_ASSET_CACHE=/target/signing-tools TILO_ASSET_OFFLINE=1
bash os/dev/install-cached-cosign.sh >/dev/null
export PATH="$(cat "$GITHUB_PATH"):$PATH"

# A board child carries private firmware; the forge's registry takes the run's stored token.
auth="$work/auth.json"
registry_login code.tilo.so "$auth"
export REGISTRY_AUTH_FILE="$auth"

export TILO_PUBLISHED_OS_IMAGE="$live_base"
export TILO_EXPECTED_OS_SOURCE_REVISION="$source_revision"
# build-custom-installer.sh's own variables, as tools/ci/publish/installer-medium.sh sets them:
# the live root the internal bundle's export shrinks, the local tag that keeps this architecture
# apart in one guest, and no EFI/PCR signer (the release signer signs a protected tag's medium).
rootfs=${TILO_INSTALLER_ROOTFS:-btrfs}
[[ ${T_COMMIT:-} =~ ^[0-9a-f]{40}$ ]] || die "T_COMMIT is not a full commit: ${T_COMMIT:-unset}"
tag="whpx-installer-$profile-${T_COMMIT:0:12}"
export TILO_DISTROBOX=
export TILO_LOCAL_TAG=$tag TILO_IMAGE="localhost/tilo:$tag"
export TILO_OUTPUT="$work/build"
export TILO_INSTALLER_IMAGE="localhost/tilo-installer:$tag" TILO_INSTALLER_OUTPUT="$work/installer"
export TILO_INSTALLER_DISK="$work/installer/qcow2/disk.qcow2"
export TILO_INSTALLER_ROOTFS=$rootfs TILO_QCOW2_COMPRESS=0
export TILO_PODMAN_BUILD_NETWORK="${TILO_PODMAN_BUILD_NETWORK:-host}"
export TILO_REQUIRE_PCR_PREDICTION=0 TILO_ALLOW_LOCAL_PCR_SIGNING=0
# The medium's own boot is unsigned, so it carries the unverified-VM marker: tilo-install honours
# it only in a VM with the org.tilo.vm-install-proof port. The preloaded target's own signature
# and provenance are checked by the installer as a release target's are.
export TILO_UNVERIFIED_VM_INSTALL=1
export TMPDIR="$work/tmp"
mkdir -p "$TMPDIR"

generation=$(bash os/dev/use-published-image.sh "$live_base")
printf 'live base %s is generation %s\n' "$live_base" "$generation"

target_dir="$work/target"
if [ "$profile" = generic ]; then
  bash os/dev/prepare-ci-installer-target.sh generic "$target" "$target_dir" "$live_base" \
    > "$out/target-verification.log"
else
  bash os/dev/prepare-ci-installer-target.sh lenovo-83ed "$target" "$target_dir" \
    > "$out/target-verification.log"
fi
grep -Fq "verified CI $profile target $target from source $source_revision" "$out/target-verification.log" \
  || die "the signed target's source differs from the pinned release source"
export TILO_INSTALLER_OFFLINE_TARGET_DIR="$target_dir"
cp "$target_dir/target.json" "$out/target.json"

bash os/dev/build-custom-installer.sh --reuse-image-installer \
  || die "the offline installer medium did not build"
[[ -s $TILO_INSTALLER_DISK ]] || die "the install disk step produced no qcow2"
[[ -f $TILO_INSTALLER_DISK.hardware.json ]] || die "the install disk step produced no media provenance"
mv "$TILO_INSTALLER_DISK" "$out/disk.qcow2"
mv "$TILO_INSTALLER_DISK.hardware.json" "$out/disk.qcow2.hardware.json"
python3 apps/imager/packaging/media-provenance.py validate "$out/disk.qcow2" aarch64 generic >/dev/null \
  || die "the medium does not match the media provenance the disk step wrote"

(cd "$out" && find . -type f ! -name artifacts.sha256 -printf '%P\n' | LC_ALL=C sort \
  | while IFS= read -r file; do sha256sum "$file"; done) > "$out/artifacts.sha256"
(cd "$out" && sha256sum --check --strict artifacts.sha256 >/dev/null) \
  || die "the artifacts do not match their own checksums"
printf 'built the %s offline installer medium from %s and %s\n' "$profile" "$live_base" "$target"
