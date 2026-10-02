#!/usr/bin/env bash
# Gate kernel-plus-userspace boot latency on a running installed image. Firmware
# timing is intentionally excluded: CI controls the Tilo kernel and userspace,
# not a customer's UEFI implementation.
#
# The numbers are only meaningful in a KVM guest, like the release gate's VM.
# os/dev/vm.sh falls back to TCG emulation on a host without usable KVM and
# warns when it does; a TCG boot's phases are not comparable with KVM or
# hardware in either direction.
set -euo pipefail

budget_seconds="${TILO_BOOT_USERSPACE_BUDGET_SEC:-20}"
case "$budget_seconds" in ''|*[!0-9]*) echo 'invalid boot budget' >&2; exit 2 ;; esac
if [ "${#budget_seconds}" -gt 3 ] \
   || [ "$budget_seconds" -lt 1 ] \
   || [ "$budget_seconds" -gt 600 ]; then
  echo 'boot budget must be between 1 and 600 seconds' >&2
  exit 2
fi

if ! systemctl is-active --quiet graphical.target; then
  echo 'graphical.target is not active' >&2
  exit 1
fi
if ! systemctl is-active --quiet greetd.service; then
  echo 'greetd.service is not active' >&2
  exit 1
fi
# greetd listens on /run/greetd-<main pid>.sock (the GREETD_SOCK it gives its
# greeter), not on a fixed path.
greetd_socket="${TILO_GREETD_SOCKET:-}"
if [ -z "$greetd_socket" ]; then
  greetd_pid="$(systemctl show --property=MainPID --value greetd.service 2>/dev/null || true)"
  greetd_socket="/run/greetd-${greetd_pid:-0}.sock"
fi
if [ ! -S "$greetd_socket" ]; then
  echo "greetd login endpoint is unavailable: $greetd_socket" >&2
  exit 1
fi

zram_sysfs="${TILO_ZRAM_SYSFS:-/sys/block/zram0}"
if [ ! -r "$zram_sysfs/comp_algorithm" ]; then
  echo 'zram0 compressor state is unavailable' >&2
  exit 1
fi
zram_algorithm="$(cat "$zram_sysfs/comp_algorithm")"
case " $zram_algorithm " in
  *' [lz4] '*) ;;
  *) echo "zram0 active compressor is not lz4: $zram_algorithm" >&2; exit 1 ;;
esac
active_swaps="$(swapon --noheadings --raw --show=NAME 2>/dev/null)"
if ! grep -Fqx /dev/zram0 <<<"$active_swaps"; then
  echo 'zram0 is not active swap' >&2
  exit 1
fi
if [ "$active_swaps" != /dev/zram0 ]; then
  echo "persistent or unexpected swap is active; Tilo permits only /dev/zram0: $active_swaps" >&2
  exit 1
fi

ntsync_device="${TILO_NTSYNC_DEVICE:-/dev/ntsync}"
if [ ! -c "$ntsync_device" ]; then
  echo "Wine/Proton ntsync device is unavailable: $ntsync_device" >&2
  exit 1
fi

# Tilo selects BFQ only for physical SATA/SCSI and NVMe disks. Release VMs use
# virtio and legitimately have no matching device, but a physical disk must
# demonstrate that the udev policy took effect rather than merely shipping it.
block_sysfs="${TILO_BLOCK_SYSFS:-/sys/block}"
for scheduler in \
  "$block_sysfs"/sd*/queue/scheduler \
  "$block_sysfs"/nvme[0-9]*n[0-9]*/queue/scheduler; do
  [ -e "$scheduler" ] || continue
  state="$(cat "$scheduler")"
  case " $state " in
    *' [bfq] '*) ;;
    *) echo "physical disk BFQ scheduler is not active at $scheduler: $state" >&2; exit 1 ;;
  esac
done

activation_timestamp() {
  local unit="$1" timestamp
  timestamp="$(systemctl show "$unit" --property=ActiveEnterTimestampMonotonic --value)"
  case "$timestamp" in ''|*[!0-9]*)
    echo "$unit has no monotonic activation timestamp" >&2
    exit 1
    ;;
  esac
  printf '%s\n' "$timestamp"
}

graphical_us="$(activation_timestamp graphical.target)"
greetd_us="$(activation_timestamp greetd.service)"
# A target may become active before its display manager has accepted work.
# Gate the later event so this measures a usable login endpoint, not merely an
# early systemd synchronization point.
entered_us="$graphical_us"
if [ "$greetd_us" -gt "$entered_us" ]; then entered_us="$greetd_us"; fi
budget_us=$((budget_seconds * 1000000))
seconds=$((entered_us / 1000000))
milliseconds=$(((entered_us % 1000000) / 1000))
# Where the time went: the manager's own initrd and userspace start points
# split the total into kernel, initrd and userspace.
boot_phases() {
  local initrd_us userspace_us
  initrd_us="$(systemctl show --property=InitRDTimestampMonotonic --value 2>/dev/null || true)"
  userspace_us="$(systemctl show --property=UserspaceTimestampMonotonic --value 2>/dev/null || true)"
  case "$initrd_us$userspace_us" in ''|*[!0-9]*) return 0 ;; esac
  [ "$initrd_us" -gt 0 ] && [ "$userspace_us" -gt "$initrd_us" ] || return 0
  awk -v i="$initrd_us" -v u="$userspace_us" -v e="$entered_us" 'BEGIN {
    printf "phases: kernel %.3fs, initrd %.3fs, userspace to greeter %.3fs\n",
      i / 1e6, (u - i) / 1e6, (e - u) / 1e6 }'
}
# The kernel phase has no units to blame: name the longest silences in this
# boot's kernel log before the initrd started, and the initramfs unpack.
kernel_gaps() {
  local log
  log="$(journalctl -k -b -o short-monotonic --no-pager 2>/dev/null)" || return 0
  [ -n "$log" ] || return 0
  # The clock can include time before the kernel ran (firmware, loading the
  # UKI): the first message's timestamp is that share.
  awk -F'[][]' 'NR == 1 { printf "kernel log starts at %.3fs\n", $2 + 0; exit }' <<<"$log"
  printf 'kernel log, longest gaps before the initrd:\n'
  awk -F'[][]' -v end="${initrd_us_for_gaps:-0}" '
    { t = $2 + 0 }
    end > 0 && t * 1e6 > end { exit }
    p != "" && t - p > 0.2 { printf "  %7.3fs before [%s]%s\n", t - p, $2, substr($3, 1, 110) }
    { p = t }' <<<"$log" | sort -rn | head -12 || true
  grep -E 'Trying to unpack|Freeing initrd memory|Run /init' <<<"$log" | cut -c1-140 || true
}
initrd_us_for_gaps="$(systemctl show --property=InitRDTimestampMonotonic --value 2>/dev/null || true)"
case "$initrd_us_for_gaps" in ''|*[!0-9]*) initrd_us_for_gaps=0 ;; esac
if [ "$entered_us" -gt "$budget_us" ]; then
  printf 'kernel+userspace reached usable greeter in %d.%03ds (budget: %ss)\n' \
    "$seconds" "$milliseconds" "$budget_seconds" >&2
  boot_phases >&2
  kernel_gaps >&2
  systemd-analyze critical-chain graphical.target >&2 || true
  systemd-analyze blame --no-pager 2>/dev/null | grep -v '\.device$' | head -20 >&2 || true
  exit 1
fi
boot_phases
printf 'kernel+userspace reached usable greeter in %d.%03ds (budget: %ss)\n' \
  "$seconds" "$milliseconds" "$budget_seconds"
