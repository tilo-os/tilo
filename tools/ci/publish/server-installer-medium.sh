#!/usr/bin/env bash
# The server installer medium (installer-medium.sh, over the OS image t built in /target/os) with
# the sealed server image SEALED (SEALED/image.oci.tar, SEALED/digest) preloaded as its target,
# into MEDIUM, and beside it the internal bundle the Imager stages onto a machine's own disk
# (os/dev/export-internal-bundle.sh), into BUNDLE. The bundle shrinks the live root to what it
# holds, which only Btrfs does, so the medium's root is Btrfs; only the run's own steps read the
# medium, so it is left uncompressed. The target's update origin is named by its digest.
#
# usage: server-installer-medium.sh SEALED MEDIUM BUNDLE
set -euo pipefail
usage='usage: server-installer-medium.sh SEALED MEDIUM BUNDLE'
sealed=${1:?$usage}
medium=${2:?$usage}
bundle=${3:?$usage}
cd "$(dirname -- "${BASH_SOURCE[0]}")/../.."
digest=$(cat "$sealed/digest")
[[ $digest =~ ^sha256:[0-9a-f]{64}$ ]] || { echo "$sealed/digest names no image" >&2; exit 1; }
export TILO_INSTALLER_TARGET_OCI=$sealed/image.oci.tar
export TILO_INSTALLER_TARGET_REPOSITORY=registry.tilo.so/tilo/tilo-host
export TILO_INSTALLER_TARGET_DIGEST=$digest
export TILO_INSTALLER_TARGET_ARCHITECTURE=x86_64
export TILO_INSTALLER_TARGET_PROFILE=generic
export TILO_INSTALLER_TARGET_UPDATE_ORIGIN=registry.tilo.so/tilo/tilo-host:dev-${digest:7:12}
export TILO_INSTALLER_ROOTFS=btrfs TILO_QCOW2_COMPRESS=0
bash tools/ci/publish/installer-medium.sh x86_64 /target/os/x86_64/image "$medium"
started=$SECONDS
rm -rf "$bundle"
mkdir -p "$(dirname -- "$bundle")"
TMPDIR=/var/tmp bash os/dev/export-internal-bundle.sh "$medium/disk.qcow2" x86_64 imager-v0.0.0 "$bundle"
echo "TILO_MEDIUM_TIMING phase=bundle seconds=$((SECONDS - started))"
