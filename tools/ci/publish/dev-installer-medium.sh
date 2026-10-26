#!/usr/bin/env bash
# A bootable development installer medium from an OS image the same run built.
#
#   dev-installer-medium.sh ARCH IMAGE_DIR OUT
#
# IMAGE_DIR is what os-image-ARCH left: the image composed from its layer actions as an OCI
# layout (oci/) and its digest. This check consumes that exact image, never a registry tag, and
# writes OUT/disk.qcow2 with its
# SHA-256, the media-provenance sidecar the canonical disk step records, and source.json naming
# the image commit and OCI digest the medium was made from.
#
# The image is an ordinary development build: its UKI seals a placeholder composefs digest, so
# it is not installable by itself. os/dev/build-custom-installer.sh is the canonical way to
# close that seal: it layers the live installer on the image, installs it through
# os/dev/disk-image.sh, and rewrites the live and target UKIs with the digest `bootc install`
# computed before the qcow2 is converted. This is the self-install path from os/dev/README.md,
# not the Windows-direct medium: it needs no capture qualifier, source assets, signing identity
# or receiver proof, and it writes nothing to the source tree.
#
# The medium is deliberately unsigned and developer-only. It is never published to a registry,
# never tagged `latest` or for a release channel, and no check here signs anything.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"

arch=${1:?usage: dev-installer-medium.sh ARCH IMAGE_DIR OUT}
in=${2:?}
out=${3:?}
case $arch in
  x86_64) oci_arch=amd64 ;;
  aarch64) oci_arch=arm64 ;;
  *) die "unknown architecture: $arch" ;;
esac
[[ $(uname -m) == "$arch" ]] || die "the $arch medium builds natively, not on $(uname -m)"

# The image is composed from layer actions keyed by what they read, so it names no commit
# itself; the medium is of this check's commit, whose run composed it.
revision=$(git rev-parse --verify HEAD) || die 'the medium names its commit: git = true'
expected_digest=$(tr -d '[:space:]' < "$in/digest") || die "$in/digest is not readable"
[[ $expected_digest =~ ^sha256:[0-9a-f]{64}$ ]] || die "$in/digest does not name an image: $expected_digest"
[[ -f $in/oci/index.json ]] || die "$in holds no OS image layout (oci/)"

rootfs=${4:-${T_INPUT_ROOTFS:-${TILO_INSTALLER_ROOTFS:-ext4}}}
case $rootfs in
  btrfs|ext4) ;;
  *) die "the live medium's root filesystem must be btrfs or ext4, not $rootfs" ;;
esac

# Root's tools in the guest: podman for the image and the installer layer, and what the
# canonical disk step installs with (os-release.sh's list, plus mtools for the ESP fallback).
dnf install -y --setopt=install_weak_deps=False podman skopeo fuse-overlayfs e2fsprogs \
  util-linux jq python3 dosfstools btrfs-progs systemd mtools qemu-img sudo curl tar gzip zstd \
  shadow-utils >/dev/null
modprobe fuse 2>/dev/null || true
modprobe loop 2>/dev/null || true

id=$(podman pull -q "oci:$in/oci") || die "could not load $in/oci"
actual_digest=$(podman image inspect --format '{{.Digest}}' "$id")
[[ $actual_digest == "$expected_digest" ]] || die "$in/oci is not the image its digest names"
[[ $(podman image inspect --format '{{.Architecture}}' "$id") == "$oci_arch" ]] \
  || die "$in/oci is not an $arch image"

# Refuse a base that cannot become this medium before spending the disk build on it. The
# installer's session plumbing (tilo-session-select, the greetd configuration) is not the base's:
# os/dev/installer_overlay.py stages it from os/usr and os/Containerfile.installer checks it there.
podman run --rm --pull=never --network=none --entrypoint /bin/sh "$id" -c '
  status=0
  for tool in /usr/libexec/tilo/tilo-installer /usr/libexec/tilo-uki-cmdline; do
    test -x "$tool" || { echo "missing: $tool ($(ls -ld "$tool" 2>&1))" >&2; status=1; }
  done
  set -- /boot/EFI/Linux/*.efi
  test "$#" -eq 1 && test -f "$1" || { echo "UKIs under /boot/EFI/Linux: $*" >&2; status=1; }
  exit "$status"
' || die "$in/oci has no graphical installer or no single sealed UKI"

source_epoch=$(podman image inspect --format '{{index .Labels "org.tilo.source-date-epoch"}}' "$id" 2>/dev/null || true)
if ! [[ $source_epoch =~ ^[1-9][0-9]*$ ]] && git cat-file -e "$revision^{commit}" 2>/dev/null; then
  source_epoch=$(git show -s --format=%ct "$revision")
fi
if [[ $source_epoch =~ ^[1-9][0-9]*$ ]]; then
  export SOURCE_DATE_EPOCH=$source_epoch
fi

mkdir -p "$out"
medium="$out/disk.qcow2"
installer_output="$out/installer"
rm -rf "$medium" "$medium.sha256" "$medium.hardware.json" "$out/source.json" "$installer_output"
# A unique local tag keeps this check apart from another architecture's images in one guest.
tag="dev-installer-medium-$arch-${revision:0:12}"
export TILO_DISTROBOX=
export TILO_LOCAL_TAG=$tag
export TILO_IMAGE="localhost/tilo:$tag"
export TILO_OUTPUT="$out/build"
export TILO_INSTALLER_IMAGE="localhost/tilo-installer:$tag"
export TILO_INSTALLER_OUTPUT=$installer_output
export TILO_INSTALLER_DISK="$installer_output/qcow2/disk.qcow2"
export TILO_INSTALLER_ROOTFS=$rootfs
# The qcow2 is the artifact and WHPX may read it too: zstd clusters keep it small, and it stays
# a normal qcow2 that QEMU and the WHPX transport can open directly.
export TILO_QCOW2_COMPRESS=1
# The bootc helper the live installer uses is compiled from a pinned public source archive in
# a nested podman build; the guest's own network is what reaches GitHub and Fedora.
export TILO_PODMAN_BUILD_NETWORK="${TILO_PODMAN_BUILD_NETWORK:-host}"
# This is not a release build and carries no EFI/PCR signer.
export TILO_REQUIRE_PCR_PREDICTION=0 TILO_ALLOW_LOCAL_PCR_SIGNING=0
# With no prediction its target can't pass the installer's authority preflight, so the medium
# carries the unverified-VM marker. tilo-install honours the marker only in a VM that has the
# org.tilo.vm-install-proof port (`vm.sh boot-installer --install-proof`, or
# `tilo-vm scenario install` with TILO_UNVERIFIED_VM_INSTALL=1); on hardware the
# preflight still runs and refuses.
export TILO_UNVERIFIED_VM_INSTALL=1
export TMPDIR="$out/tmp"
mkdir -p "$TMPDIR"
podman tag "$id" "$TILO_IMAGE"

if ! bash os/dev/build-custom-installer.sh --reuse-image-installer; then
  # The disk step keeps bootc's own words in a log beside the disk, which t does not keep.
  log="$installer_output/qcow2/.install.log"
  [[ ! -s $log ]] || { echo "--- the last lines of $log:" >&2; tail -n 120 "$log" >&2; }
  die "the installer disk step failed"
fi

[[ -s "$TILO_INSTALLER_DISK" ]] || die "the installer disk step produced no qcow2"
[[ -f "$TILO_INSTALLER_DISK.hardware.json" ]] \
  || die "the installer disk step produced no media provenance"
mv "$TILO_INSTALLER_DISK" "$medium"
mv "$TILO_INSTALLER_DISK.hardware.json" "$medium.hardware.json"
rm -rf "$installer_output" "$TILO_OUTPUT" "$TMPDIR"

# The canonical provenance validator reads the finished qcow2 again and names its installer
# image and update origin. It records build provenance, never a signature or boot proof.
python3 apps/imager/packaging/media-provenance.py validate "$medium" "$arch" generic >/dev/null \
  || die "the medium does not match the media provenance the disk step recorded"

sha256sum "$medium" | awk '{print $1 "  disk.qcow2"}' > "$medium.sha256"
python3 - "$medium" "$medium.hardware.json" "$out/source.json" "$arch" "$revision" "$actual_digest" "$rootfs" <<'PY'
import hashlib
import json
from pathlib import Path
import sys

medium, hardware, destination, arch, revision, digest, rootfs = sys.argv[1:]
medium_path = Path(medium)
with medium_path.open("rb") as stream:
    medium_digest = hashlib.file_digest(stream, "sha256").hexdigest()
record = json.loads(Path(hardware).read_text())
source = {
    "schema": "tilo.dev-installer-medium.v1",
    "architecture": arch,
    "source_commit": revision,
    "source_image_digest": digest,
    "source_image_id": record["source_image_id"],
    "hardware_profile": record["hardware_profile"],
    "live_rootfs": rootfs,
    "medium_file": "disk.qcow2",
    "medium_sha256": medium_digest,
    "medium_size": medium_path.stat().st_size,
    "media_provenance_sha256": hashlib.sha256(Path(hardware).read_bytes()).hexdigest(),
    "trust": "development-unsigned",
    "release": False,
}
Path(destination).write_text(json.dumps(source, indent=2, sort_keys=True) + "\n")
PY
printf 'built %s of %s from %s\n' "$medium" "$revision" "$actual_digest"
