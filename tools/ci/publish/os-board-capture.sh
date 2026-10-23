#!/usr/bin/env bash
# The crash capture of a board image os-image.sh board built, checked in the image itself:
#
#   os-board-capture.sh BOARD IN    IN holds image.oci.tar from os-board-BOARD
#
# Reads the kernel configuration, the hardware profile, the UKI (its command line, device tree
# and initramfs) out of the image, then boots the UKI's kernel and initramfs under KVM, makes
# the initrd fail and warm-resets (os/tests/board_initrd_pstore_vm.py), so the failure has to
# reach the next boot's pstore console record. Runs as root in a micro-VM step, like
# kernel-acceptance.sh, installing what it needs.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"

board=${1:-}
in=${2:-}
[[ $board == lenovo-83ed && -n $in ]] || die 'usage: os-board-capture.sh lenovo-83ed IN'
[[ $(uname -m) == aarch64 ]] || die 'the board image is checked natively on aarch64'
[[ -e /dev/kvm ]] || die 'no /dev/kvm'

dnf install -y --setopt=install_weak_deps=False podman qemu-system-aarch64-core dracut \
  python3 binutils zstd >/dev/null
podman=(podman --cgroup-manager=cgroupfs --events-backend=file)
work=$scratch/board-capture
rm -rf "$work"
mkdir -p "$work/root" "$work/uki"
id=$("${podman[@]}" pull --quiet "oci-archive:$in/image.oci.tar")
container=$("${podman[@]}" create "$id" /unused)
for path in /boot /usr/lib/tilo /etc/dracut.conf.d /usr/lib/bootc/kargs.d \
  /usr/lib/systemd/system/tilo-pstore-console.service; do
  mkdir -p "$work/root$(dirname "$path")"
  "${podman[@]}" cp "$container:$path" "$work/root$(dirname "$path")/"
done
# The profile names its device tree under the one kernel's modules directory.
kver=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["devicetree"].split("/")[4])' \
  "$work/root/usr/lib/tilo/hardware-profile.json")
"${podman[@]}" cp "$container:/usr/lib/modules/$kver/config" "$work/config"
# The PMIC power-on/reset reason logging (os/kernel/patches/0022) lives in the qcom-pon module.
"${podman[@]}" cp "$container:/usr/lib/modules/$kver/kernel/drivers/power/reset/qcom-pon.ko.zst" "$work/"
"${podman[@]}" rm "$container" >/dev/null

problems=()
need() { grep -qxF -- "$2" "$1" || problems+=("$(basename "$1") lacks $2"); }
# The kernel: pstore's RAM backend and console, the hung-task and buddy hard-lockup detectors.
for option in CONFIG_PSTORE=y CONFIG_PSTORE_RAM=y CONFIG_PSTORE_CONSOLE=y CONFIG_DETECT_HUNG_TASK=y \
  CONFIG_HARDLOCKUP_DETECTOR_BUDDY=y CONFIG_ARM_SBSA_WATCHDOG=y; do
  grep -qxF "$option" "$work/config" || grep -qx "${option%=y}=m" "$work/config" \
    || problems+=("kernel $kver lacks $option")
done
printf 'kernel %s: %s\n' "$kver" "$(grep -E '^CONFIG_(PSTORE|DETECT_HUNG_TASK|HARDLOCKUP_DETECTOR_BUDDY|ARM_SBSA_WATCHDOG)[A-Z_]*=' "$work/config" | tr '\n' ' ')"

zstd -dcq "$work/qcom-pon.ko.zst" | grep -qa '0x%04x+0x%02x: unreadable (%d)' \
  || problems+=("qcom-pon does not log the PMIC's power-on and reset reasons")

shopt -s nullglob
ukis=("$work"/root/boot/EFI/Linux/*.efi)
((${#ukis[@]} == 1)) || die "expected one UKI in /boot/EFI/Linux, found ${#ukis[@]}"
python3 -I tools/ci/publish/uki_sections.py "${ukis[0]}" "$work/uki"
cmdline=$(tr -d '\0' < "$work/uki/cmdline")
printf 'UKI command line: %s\n' "$cmdline"
for argument in systemd.watchdog_sec=30 sbsa_gwdt.action=1 softlockup_panic=1 nmi_watchdog=panic \
  printk.devkmsg=on rd.systemd.journald.forward_to_kmsg=1 rd.systemd.journald.max_level_kmsg=info \
  systemd.tpm2_wait=0; do
  [[ " $cmdline " == *" $argument "* ]] || problems+=("the UKI command line lacks $argument")
done
[[ " $cmdline " != *" hung_task_panic=1 "* ]] || problems+=("the UKI command line panics on a hung task")
[[ " $cmdline " != *" systemd.journald.forward_to_kmsg"* ]] || problems+=("the host's journald forwards to kmsg")
grep -qa 'ramoops@c00000000' "$work/uki/dtb" || problems+=("the UKI's device tree has no ramoops region")
need "$work/root/usr/lib/systemd/system/tilo-pstore-console.service" IgnoreOnIsolate=yes
grep -q '"target_update_ref": "registry.tilo.so/tilo/tilo-vm:pr-4433-d7d1575-lenovo-83ed"' \
  "$work/root/usr/lib/tilo/hardware-profile.json" || problems+=("the update origin is not the laptop's current one")
grep '"target_update_ref"' "$work/root/usr/lib/tilo/hardware-profile.json"

# The initramfs the UKI carries, as the initrd sees it.
listing=$(lsinitrd "$work/uki/initrd")
for path in usr/lib/systemd/system/tilo-pstore-console.service \
  usr/lib/systemd/system/initrd.target.d/50-tilo-pstore-console.conf usr/bin/dmesg; do
  grep -q " $path\$\| $path -> " <<<"$listing" || problems+=("the UKI's initramfs lacks $path")
done
printf 'initramfs drop-in:\n%s\n' "$(lsinitrd -f usr/lib/systemd/system/initrd.target.d/50-tilo-pstore-console.conf "$work/uki/initrd")"

if ((${#problems[@]})); then
  printf 'board capture: %s\n' "${problems[@]}" >&2
  exit 1
fi
printf 'board image: kernel configuration, command line, device tree and initramfs carry the crash capture\n'
python3 -I os/tests/board_initrd_pstore_vm.py "$work/uki"
