#!/usr/bin/env bash
# A bootable development installer medium from an OS image the same run built,
# for any image it preloads as the install target.
#
#   installer-medium.sh ARCH IMAGE_DIR OUT
#
# IMAGE_DIR is what os/image/build-layer.sh compose left: the image composed from its layer
# actions as an OCI layout (oci/) and its digest. The medium is made from that exact image,
# never a registry tag, and writes OUT/disk.qcow2 with its
# SHA-256, the media-provenance sidecar the canonical disk step records, and source.json naming
# the OCI digest the medium was made from. It reads no Git: the image names no commit, so a medium
# is keyed by the images it is made from and served while they are as they were.
#
# A preloaded target is optional and is any OCI archive the same run built
# (TILO_INSTALLER_TARGET_OCI), with TILO_INSTALLER_TARGET_REPOSITORY,
# _DIGEST, _ARCHITECTURE, _PROFILE and _UPDATE_ORIGIN naming it. The target's
# architecture may differ from the live medium's; the transport records every
# field explicitly, and source.json carries the same identity.
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
# never tagged `latest` or for a release channel, and nothing here signs anything.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"

arch=${1:?usage: installer-medium.sh ARCH IMAGE_DIR OUT}
in=${2:?}
out=${3:?}
case $arch in
  x86_64) oci_arch=amd64 ;;
  aarch64) oci_arch=arm64 ;;
  *) die "unknown architecture: $arch" ;;
esac
[[ $(uname -m) == "$arch" ]] || die "the $arch medium builds natively, not on $(uname -m)"

expected_digest=$(tr -d '[:space:]' < "$in/digest") || die "$in/digest is not readable"
[[ $expected_digest =~ ^sha256:[0-9a-f]{64}$ ]] || die "$in/digest does not name an image: $expected_digest"
[[ -f $in/oci/index.json ]] || die "$in holds no OS image layout (oci/)"

rootfs=${TILO_INSTALLER_ROOTFS:-ext4}
case $rootfs in
  btrfs|ext4) ;;
  *) die "the live medium's root filesystem must be btrfs or ext4, not $rootfs" ;;
esac

# How long each part took, for finding what a medium spends its time on (as build-layer.sh's).
phase_started=$SECONDS phase_name=start
phase() {
  printf 'TILO_MEDIUM_TIMING phase=%s seconds=%s\n' "$phase_name" "$((SECONDS - phase_started))"
  phase_name=$1 phase_started=$SECONDS
}
trap 'phase end' EXIT

# Root's tools in the guest: podman for the image and the installer layer, and what the
# canonical disk step installs with (os-release.sh's list, plus mtools for the ESP fallback).
# Only what the image lacks: asking dnf for installed packages still fetches every repository's
# metadata.
phase tools
tools=(podman skopeo fuse-overlayfs e2fsprogs util-linux jq python3 dosfstools btrfs-progs systemd
  mtools qemu-img sudo curl tar gzip zstd shadow-utils)
missing=()
for tool in "${tools[@]}"; do rpm -q "$tool" >/dev/null 2>&1 || missing+=("$tool"); done
((${#missing[@]} == 0)) \
  || dnf install -y --setopt=install_weak_deps=False "${missing[@]}" >/dev/null
modprobe fuse 2>/dev/null || true
modprobe loop 2>/dev/null || true

phase load-image
id=$(load_for_disk "$in/oci")
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
if [[ $source_epoch =~ ^[1-9][0-9]*$ ]]; then
  export SOURCE_DATE_EPOCH=$source_epoch
fi

mkdir -p "$out"
medium="$out/disk.qcow2"
installer_output="$out/installer"
rm -rf "$medium" "$medium.sha256" "$medium.hardware.json" "$out/source.json" "$installer_output"

# An optional preloaded target makes this that image's installer: any image the
# same run built, staged as the unsigned development transport the VM installer
# admits, with its repository, digest, architecture and update origin written
# out explicitly rather than inferred from a hardware profile.
target_json=""
if [ -n "${TILO_INSTALLER_TARGET_OCI:-}" ]; then
  : "${TILO_INSTALLER_TARGET_REPOSITORY:?TILO_INSTALLER_TARGET_REPOSITORY is required with a target}"
  : "${TILO_INSTALLER_TARGET_DIGEST:?TILO_INSTALLER_TARGET_DIGEST is required with a target}"
  : "${TILO_INSTALLER_TARGET_ARCHITECTURE:?TILO_INSTALLER_TARGET_ARCHITECTURE is required with a target}"
  : "${TILO_INSTALLER_TARGET_PROFILE:?TILO_INSTALLER_TARGET_PROFILE is required with a target}"
  : "${TILO_INSTALLER_TARGET_UPDATE_ORIGIN:?TILO_INSTALLER_TARGET_UPDATE_ORIGIN is required with a target}"
  phase stage-target
  target_dir="$out/target"
  rm -rf "$target_dir"
  mkdir -p "$target_dir"
  skopeo copy --preserve-digests "oci-archive:$TILO_INSTALLER_TARGET_OCI" \
    "oci:$target_dir/oci:target" >/dev/null \
    || die "could not stage $TILO_INSTALLER_TARGET_OCI as the preloaded target"
  # The commit the target names, if any: a dev image's build names its commit, the server
  # image's reads no Git and names none.
  target_revision=$(skopeo inspect --config "oci:$target_dir/oci:target" \
    | jq -r '.config.Labels["org.opencontainers.image.revision"] // ""')
  python3 - "$target_dir/target.json" \
    "$TILO_INSTALLER_TARGET_REPOSITORY" "$TILO_INSTALLER_TARGET_DIGEST" \
    "$TILO_INSTALLER_TARGET_ARCHITECTURE" "$TILO_INSTALLER_TARGET_PROFILE" \
    "$TILO_INSTALLER_TARGET_UPDATE_ORIGIN" "$target_revision" "${source_epoch:-1}" <<'PY'
import json
from pathlib import Path
import sys

target, repository, digest, architecture, profile, origin, revision, run = sys.argv[1:]
Path(target).write_text(json.dumps({
    "schema": "tilo.installer-dev-target.v1",
    "repository": repository,
    "target_digest": digest,
    "architecture": architecture,
    "hardware_profile": profile,
    "update_origin": origin,
    "source_revision": revision,
    "ci_run_id": run,
}, sort_keys=True) + "\n")
PY
  python3 os/dev/installer-offline-target.py verify "$target_dir" --development >/dev/null \
    || die "the preloaded target is not a verified development transport"
  target_json="$target_dir/target.json"
  export TILO_INSTALLER_OFFLINE_TARGET_DIR="$target_dir"
  export TILO_INSTALLER_DEV_TARGET=1
fi
# A unique local tag keeps the medium apart from another architecture's images in one guest.
tag="dev-installer-medium-$arch-${actual_digest:7:12}"
export TILO_DISTROBOX=
export TILO_LOCAL_TAG=$tag
export TILO_IMAGE="localhost/tilo:$tag"
export TILO_OUTPUT="$out/build"
export TILO_INSTALLER_IMAGE="localhost/tilo-installer:$tag"
export TILO_INSTALLER_OUTPUT=$installer_output
export TILO_INSTALLER_DISK="$installer_output/qcow2/disk.qcow2"
export TILO_INSTALLER_ROOTFS=$rootfs
# The qcow2 is the artifact and WHPX may read it too: zstd clusters keep it small, and it stays
# a normal qcow2 that QEMU and the WHPX transport can open directly. A medium only this run's
# own steps read sets TILO_QCOW2_COMPRESS=0: compressing it takes longer than its install.
export TILO_QCOW2_COMPRESS=${TILO_QCOW2_COMPRESS:-1}
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

phase installer-and-disk
# Its messages stamped with the seconds since this phase began: the disk step's parts (the
# installer layer, the scratch registry, each install pass, the qcow2) are most of a medium.
stamp() { local line; while IFS= read -r line; do printf '[+%ss] %s\n' "$((SECONDS - phase_started))" "$line"; done; }
if ! bash os/dev/build-custom-installer.sh --reuse-image-installer 2>&1 | stamp; then
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
phase provenance
python3 apps/imager/packaging/media-provenance.py validate "$medium" "$arch" generic >/dev/null \
  || die "the medium does not match the media provenance the disk step recorded"

sha256sum "$medium" | awk '{print $1 "  disk.qcow2"}' > "$medium.sha256"
python3 - "$medium" "$medium.hardware.json" "$out/source.json" "$arch" "$actual_digest" "$rootfs" "$target_json" <<'PY'
import hashlib
import json
from pathlib import Path
import sys

medium, hardware, destination, arch, digest, rootfs, target = sys.argv[1:]
medium_path = Path(medium)
with medium_path.open("rb") as stream:
    medium_digest = hashlib.file_digest(stream, "sha256").hexdigest()
record = json.loads(Path(hardware).read_text())
target = json.loads(Path(target).read_text()) if target else None
source = {
    "schema": "tilo.dev-installer-medium.v1",
    "architecture": arch,
    "source_image_digest": digest,
    "source_image_id": record["source_image_id"],
    "hardware_profile": record["hardware_profile"],
    "live_rootfs": rootfs,
    "medium_file": "disk.qcow2",
    "medium_sha256": medium_digest,
    "medium_size": medium_path.stat().st_size,
    "media_provenance_sha256": hashlib.sha256(Path(hardware).read_bytes()).hexdigest(),
    "target": None if target is None else {
        key: target[key]
        for key in ("repository", "target_digest", "architecture",
                    "hardware_profile", "update_origin")
    },
    "trust": "development-unsigned",
    "release": False,
}
Path(destination).write_text(json.dumps(source, indent=2, sort_keys=True) + "\n")
PY
printf 'built %s from %s\n' "$medium" "$actual_digest"
