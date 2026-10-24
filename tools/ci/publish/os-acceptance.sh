#!/usr/bin/env bash
# The OS image and its dev image as devices get them, as root in a micro-VM step with a
# /dev/kvm of its own. Nothing here holds a secret or publishes.
#
#   os-acceptance.sh ARCH IMAGE DEV OUT   IMAGE: os-image-ARCH's layout, DEV: os-dev-image-ARCH's;
#                                         reports and logs into OUT
#
# The dev image is installed onto a disk (os/dev/build-image.sh with TILO_SKIP_BUILD=1; it carries sshd
# and tilo-control, which the gates drive the guests with), kept in OUT/disk for
# publish-os-dev-ARCH, and three tracks run side by side, each booting throwaway overlays of that
# disk or the OS image's own UKI:
#
#   devices  a joined device signs in (os/dev/test-joined-device-startup.sh); nearby pairing,
#            a third device through one without the identity key, sibling unlock and, on
#            x86_64, wake (os/dev/test-nearby-pairing.sh); device-admin reach through a relay
#            (os/dev/test-device-admin-reach.sh)
#   desktop  the installed desktop from the sign-in screen on, ending with a live update of a
#            newer image (os/dev/test-installed-desktop.sh)
#   splash   the boot splash on a panel larger than the firmware's mode (os/dev/check-boot-splash.py)
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"

arch=${1:?usage: os-acceptance.sh ARCH IMAGE DEV OUT}
image=${2:?}
dev=${3:?}
out=${4:?}
[[ $(uname -m) == "$arch" ]] || die "the $arch image is checked natively, not on $(uname -m)"
case $arch in
  x86_64) qemu=(edk2-ovmf qemu-system-x86-core qemu-device-display-virtio-vga qemu-device-display-virtio-gpu) ;;
  aarch64) qemu=(edk2-aarch64 qemu-system-aarch64-core qemu-device-display-virtio-gpu qemu-device-display-virtio-gpu-pci) ;;
  *) die "unknown architecture $arch" ;;
esac
for layout in "$image" "$dev"; do
  [[ -f $layout/oci/index.json ]] || die "$layout holds no image layout (oci/)"
done
[[ -c /dev/kvm ]] || die 'no /dev/kvm: the check needs requires = ["/dev/kvm"]'

missing=()
for p in dosfstools btrfs-progs systemd-container binutils socat qemu-img ipxe-roms-qemu sshpass \
  openssh-clients jq cryptsetup mtools zstd procps-ng "${qemu[@]}"; do
  rpm -q "$p" >/dev/null 2>&1 || missing+=("$p")
done
command -v podman >/dev/null || missing+=(podman skopeo fuse-overlayfs e2fsprogs util-linux python3)
((${#missing[@]} == 0)) || dnf install -y --setopt=install_weak_deps=False "${missing[@]}" >/dev/null
modprobe fuse 2>/dev/null || true
modprobe loop 2>/dev/null || true
use_cosign
rm -rf "$out"
mkdir -p "$out"
status=0

# The disk publish-os-dev-ARCH publishes once every gate passed: never booted itself (the gates
# boot overlays of it), uncompressed so they read it fast (publication compresses it), following
# the dev image it is published beside.
revision=$(git rev-parse --verify HEAD) || die 'the disk names its commit: git = true'
id=$(podman pull -q "oci:$dev/oci")
[[ $(podman image inspect --format '{{.Digest}}' "$id") == "$(cat "$dev/digest")" ]] || die "$dev/oci is not the image $dev/digest names"
podman tag "$id" localhost/tilo-vm:ci
export TILO_DISTROBOX= TILO_VM_ARCH=$arch TMPDIR=/var/tmp TILO_USER=quickuser TILO_PASS=tilo
mkdir -p "$out/disk"
if [[ $arch == aarch64 ]]; then export TILO_DEV_DIRECT_BUNDLE_DIR=$out/disk/direct-boot; fi
TILO_IMAGE=localhost/tilo-vm:ci TILO_SKIP_BUILD=1 TILO_OUTPUT=$scratch/dev-disk TILO_RUN=$scratch/dev-disk-run \
  TILO_DISK_SIZE=64G TILO_TARGET_IMGREF="registry.tilo.so/tilo/tilo-vm:main-${revision:0:7}-ci" \
  bash os/dev/build-image.sh > "$out/install.log" 2>&1 \
  || { tail -n 80 "$out/install.log"; die 'the dev image does not install onto a disk'; }
unset TILO_DEV_DIRECT_BUNDLE_DIR
# The update the live-update gate stages: the dev image with one file added, served by the
# registry the install left running, which the guests reach as the host (10.0.2.2).
probe=/usr/share/tilo/live-update-probe
probe_text="live update of ${revision:0:12}"
mkdir -p "$scratch/live-update"
printf '%s\n' "$probe_text" > "$scratch/live-update/probe"
printf 'FROM localhost/tilo-vm:ci\nCOPY probe %s\n' "$probe" > "$scratch/live-update/Containerfile"
podman build -q -t localhost/tilo-vm:live-update-check "$scratch/live-update" >/dev/null
registry_port=$(podman port tilo-disk-registry 5000/tcp | head -n 1)
registry_port=${registry_port##*:}
[[ $registry_port =~ ^[0-9]+$ ]] || die 'the install left no local registry for the live-update gate'
skopeo copy -q --dest-tls-verify=false containers-storage:localhost/tilo-vm:live-update-check \
  "docker://localhost:$registry_port/tilo-vm:live-update-check"
live_update=(--live-update "10.0.2.2:$registry_port/tilo-vm" registry.tilo.so/tilo/tilo-vm:live-update-check "$probe" "$probe_text")
podman rmi -f localhost/tilo-vm:ci localhost/tilo-vm:live-update-check >/dev/null
disk=$out/disk/disk.qcow2
mv "$scratch/dev-disk/qcow2/disk.qcow2" "$disk"
printf '%s\n' "$revision" > "$out/disk/revision"
cp "$dev/digest" "$out/disk/image-digest"

# A host without a usable render node boots the guests on the 2D virtio display; the
# compositor paints on the CPU there, and every gate must pass either way.
export TILO_VM_DISPLAY=2d
for node in /dev/dri/renderD*; do
  [[ -c $node && -r $node && -w $node ]] && { TILO_VM_DISPLAY=gl; break; }
done

wake=()
if [[ $arch == x86_64 ]]; then wake=(--wake); fi
devices() {
  local rc=0
  bash os/dev/test-joined-device-startup.sh "$disk" --out "$out/joined-device" || rc=1
  bash os/dev/test-nearby-pairing.sh "$disk" --out "$out/nearby-pairing" --unlock "${wake[@]}" || rc=1
  bash os/dev/test-device-admin-reach.sh "$disk" --out "$out/device-admin-reach" || rc=1
  return "$rc"
}
desktop() {
  bash os/dev/test-installed-desktop.sh "$disk" --out "$out/installed-desktop" --base-digest "$(cat "$image/digest")" \
    "${live_update[@]}"
}
splash() {
  python3 os/dev/check-boot-splash.py --arch "$arch" --out "$out/boot-splash" "oci:$image/oci:tilo"
}
tracks=(devices desktop splash)
pids=()
for track in "${tracks[@]}"; do
  ( set -o pipefail; "$track" 2>&1 | tee "$out/$track.log" | sed -u "s/^/[$track] /" ) &
  pids+=("$!")
done
for i in "${!tracks[@]}"; do
  if wait "${pids[$i]}"; then
    echo "ok    ${tracks[$i]}"
  else
    echo "FAIL  ${tracks[$i]} (its log: $out/${tracks[$i]}.log)"
    status=1
  fi
done
exit "$status"
