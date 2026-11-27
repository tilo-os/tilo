#!/usr/bin/env bash
# The OS image and its dev image as devices get them, as root in a micro-VM step with a
# /dev/kvm of its own. Nothing here holds a secret or publishes. A release's check, and the dev
# disk's; main's dev images need only os-dev-check-ARCH.
#
#   os-acceptance.sh ARCH IMAGE DISK OUT   IMAGE: os-image-ARCH's layout, DISK: os-dev-disk-ARCH's
#                                          output (the dev image installed); reports and logs into
#                                          OUT
#
# Three tracks run side by side, each booting throwaway overlays of the installed disk or the OS
# image's own UKI:
#
#   devices  a computer that joined a person with a ticket signs in to its desktop
#            (os/dev/test-joined-device-startup.sh); the ticket comes from a second
#            overlay of the same disk, and both share one private LAN
#   desktop  the installed desktop from the sign-in screen on (os/dev/test-installed-desktop.sh),
#            whose guest-side checks are os/tests/boot_units_check.sh,
#            os/tests/projection-boot-check.sh, os/tests/h264-boot-check.py,
#            os/tests/session-context-check.sh, os/tests/input-hotplug-check.sh,
#            os/tests/installed-admin-actions.sh, os/tests/installed_atspi_apps.sh,
#            os/tests/native_atspi_smoke.sh, os/tests/recovery-boot-check.sh,
#            os/tests/booted_app_store.sh, os/tests/booted_app_update.sh and
#            os/tests/booted_flatpak_lifecycle.sh
#   splash   the boot splash on a panel larger than the firmware's mode (os/dev/check-boot-splash.py)
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"

arch=${1:?usage: os-acceptance.sh ARCH IMAGE DISK OUT}
image=${2:?}
disk=${3:?}/disk/disk.qcow2
out=${4:?}
[[ $(uname -m) == "$arch" ]] || die "the $arch image is checked natively, not on $(uname -m)"
case $arch in
  x86_64) qemu=(edk2-ovmf qemu-system-x86-core qemu-device-display-virtio-vga qemu-device-display-virtio-gpu) ;;
  aarch64) qemu=(edk2-aarch64 qemu-system-aarch64-core qemu-device-display-virtio-gpu qemu-device-display-virtio-gpu-pci) ;;
  *) die "unknown architecture $arch" ;;
esac
[[ -f $image/oci/index.json ]] || die "$image holds no image layout (oci/)"
[[ -s $disk ]] || die "no installed disk at $disk"
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

export TILO_VM_ARCH=$arch TMPDIR=/var/tmp TILO_USER=tester TILO_PASS=tilo

# A host without a usable render node boots the guests on the 2D virtio display; the
# compositor paints on the CPU there, and every check must pass either way.
export TILO_VM_DISPLAY=2d
for node in /dev/dri/renderD*; do
  [[ -c $node && -r $node && -w $node ]] && { TILO_VM_DISPLAY=gl; break; }
done

devices() {
  bash os/dev/test-joined-device-startup.sh "$disk" --out "$out/joined-device"
}
desktop() {
  bash os/dev/test-installed-desktop.sh "$disk" --out "$out/installed-desktop" --base-digest "$(cat "$image/digest")"
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
# A failed check keeps no outputs, so the guests' own account of a failure goes to the log:
# the end of each session's journal and compositor log the checks collected,
# without the periodic frame counters and SSH logins that would fill it.
if ((status != 0)); then
  while IFS= read -r -d '' evidence; do
    printf '\n--- %s (last 80 lines of what it said)\n' "${evidence#"$out"/}"
    grep -avE '^(paint-stats|frames)[:=]|sshd-session\[' "$evidence" | tail -n 80
  done < <(find "$out" -type f \( -name '*session-journal.txt' -o -name '*compositor-session.log' \) -print0 | sort -z)
  # A frozen compositor's record opens with its main thread, the one that froze.
  while IFS= read -r -d '' evidence; do
    printf '\n--- %s (first 80 lines)\n' "${evidence#"$out"/}"
    head -n 80 "$evidence"
  done < <(find "$out" -type f -name '*compositor-freeze.log' -size +0 -print0 | sort -z)
fi
exit "$status"
