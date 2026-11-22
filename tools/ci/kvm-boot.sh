#!/usr/bin/env bash
# Boot the image's own kernel twice in the job sandbox, once on /dev/kvm and once on QEMU's
# software emulator, and read how long each takes to switch clocksource. A guest's own dmesg
# time is its measure of the acceleration it got: the emulated boot reaches the switch several
# times later. That is how this check says the sandbox hands its guests real hardware before a
# Tilo disk's boot, minutes long, would.
#
#   kvm-boot.sh [OUT]
#
# It needs `/dev/kvm` (requires = ["/dev/kvm"]) and runs in image-os-vm-ARCH, which carries QEMU
# and a kernel. Nothing here holds a secret or publishes.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/publish/lib.sh"
cd "$repo_root"
out=${1:-$scratch/kvm-boot}
[[ -c /dev/kvm ]] || die 'no /dev/kvm: the check needs requires = ["/dev/kvm"]'
command -v qemu-system-x86_64 >/dev/null || die 'the image carries no qemu-system-x86_64'
kernel=$(ls -1 /usr/lib/modules/*/vmlinuz 2>/dev/null | sort -V | tail -n 1)
[[ -f $kernel ]] || die 'the image carries no kernel to boot: /usr/lib/modules/*/vmlinuz is empty'
rm -rf "$out"
mkdir -p "$out"
printf 'kernel: %s\n' "$kernel"

# Boot it with ACCEL, leaving OUT/ACCEL-serial.log and OUT/ACCEL-qemu.err, and print the seconds
# the guest's own dmesg gives the clocksource switch.
switch_seconds() {
  local accel=$1 serial=$out/$1-serial.log err=$out/$1-qemu.err line seconds cpu=host
  # TCG has no host CPU model to pass through.
  [[ $accel == tcg ]] && cpu=max
  timeout 300 qemu-system-x86_64 -accel "$accel" -cpu "$cpu" -m 1024 -smp 1 \
    -kernel "$kernel" -append 'console=ttyS0 panic=1' \
    -display none -serial "file:$serial" -no-reboot 2>"$err" || true
  if grep -qiE 'could not access kvm|KVM (init|initialization) failed|kvm unavailable' "$err"; then
    cat "$err" >&2
    die 'QEMU did not get KVM: this sandbox boots guests without hardware acceleration'
  fi
  [[ -s $serial ]] || { cat "$err" >&2; die "the $accel guest printed nothing on its serial line"; }
  line=$(grep -iE 'clocksource: switched to' "$serial" | tail -n 1 || true)
  [[ -n $line ]] || { tail -n 40 "$serial" >&2; die "the $accel guest never switched clocksource"; }
  seconds=$(sed -E 's/^\[[[:space:]]*([0-9]+\.[0-9]+)\].*/\1/' <<<"$line")
  [[ $seconds =~ ^[0-9]+\.[0-9]+$ ]] || die "cannot read the switch's dmesg time from: $line"
  printf '%s: clocksource switch at %ss of guest time: %s\n' "$accel" "$seconds" "$line" >&2
  printf '%s' "$seconds"
}

kvm=$(switch_seconds kvm)
grep -qi 'Hypervisor detected: KVM' "$out/kvm-serial.log" \
  || die 'the KVM guest does not report a KVM hypervisor: QEMU ran it without hardware acceleration'
emulated=$(switch_seconds tcg)
printf 'kvm %ss, software emulator %ss\n' "$kvm" "$emulated"
# An emulated clock reaches the switch clearly later; the ratio is above three here, and the
# margin leaves a busy node's noise out of it. A guest without acceleration is no faster than
# the emulator, which the hypervisor line above has already refused.
awk -v k="$kvm" -v e="$emulated" 'BEGIN { exit !(e > 1.5 * k) }' \
  || die "the KVM guest's switch (${kvm}s) is not clearly faster than the emulator's (${emulated}s): it ran without acceleration"
awk -v k="$kvm" 'BEGIN { exit !(k < 3.0) }' \
  || die "the KVM guest's switch came at ${kvm}s, near the emulator's: the guest has no usable acceleration"
