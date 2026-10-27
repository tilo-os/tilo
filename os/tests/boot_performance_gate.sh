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
# An active greetd that systemd had to restart got there through a failed
# start. Five failures in a row stop it for good: the splash has already quit,
# nothing owns the display, and the panel stays dark while the rest of the
# system (sshd included) keeps running.
greetd_restarts="$(systemctl show --property=NRestarts --value greetd.service 2>/dev/null || true)"
case "$greetd_restarts" in
  0) ;;
  ''|*[!0-9]*) echo 'greetd.service restart count is unavailable' >&2; exit 1 ;;
  *) echo "greetd.service was restarted $greetd_restarts times before the greeter came up" >&2; exit 1 ;;
esac

# The greeter must have lit what it drew on. A connected connector whose CRTC
# is off, or a backlight at zero, is a black screen that every other check
# here would pass.
drm_sysfs="${TILO_DRM_SYSFS:-/sys/class/drm}"
connected=0
lit=0
for connector in "$drm_sysfs"/card*-*; do
  [ -r "$connector/status" ] || continue
  [ "$(cat "$connector/status")" = connected ] || continue
  connected=$((connected + 1))
  if [ "$(cat "$connector/enabled" 2>/dev/null)" = enabled ] \
     && [ "$(cat "$connector/dpms" 2>/dev/null)" = On ]; then
    lit=$((lit + 1))
  fi
done
if [ "$connected" -gt 0 ] && [ "$lit" -eq 0 ]; then
  echo "no connected display is lit at the greeter ($connected connected)" >&2
  exit 1
fi
backlight_sysfs="${TILO_BACKLIGHT_SYSFS:-/sys/class/backlight}"
for backlight in "$backlight_sysfs"/*; do
  [ -r "$backlight/brightness" ] || continue
  brightness="$(cat "$backlight/brightness")"
  power="$(cat "$backlight/bl_power" 2>/dev/null || echo 0)"
  if [ "$brightness" = 0 ] || [ "$power" != 0 ]; then
    echo "backlight ${backlight##*/} is dark at the greeter (brightness $brightness, bl_power $power)" >&2
    exit 1
  fi
done

# A device unit that never appears holds every unit ordered after it for the
# whole device timeout, under the splash and with nothing on screen saying
# why. A firmware TPM the kernel has no driver for did this twice per boot.
boot_log="$(journalctl -b -o short-monotonic --no-pager 2>/dev/null)" || {
  echo "this boot's journal is unreadable" >&2
  exit 1
}
device_timeouts="$(grep -E 'systemd\[1\]: Timed out waiting for device ' <<<"$boot_log" \
  | sed 's/.*Timed out waiting for device //' | sort -u || true)"
if [ -n "$device_timeouts" ]; then
  printf 'this boot waited out a device that never appeared:\n' >&2
  while IFS= read -r device; do printf '  %s\n' "$device"; done <<<"$device_timeouts" >&2
  case "$device_timeouts" in
    *tpm*) printf 'TPM support: %s\n' "$(systemd-analyze has-tpm2 2>/dev/null | tr '\n' ' ')" >&2 ;;
  esac
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
# The initrd phase: the longest silences between its start and switch-root,
# from the journal (the gate user reads it through wheel). systemd and its
# units log there in real time, so a gap names what the boot waited on.
initrd_gaps() {
  local initrd_us userspace_us log
  initrd_us="$(systemctl show --property=InitRDTimestampMonotonic --value 2>/dev/null || true)"
  userspace_us="$(systemctl show --property=UserspaceTimestampMonotonic --value 2>/dev/null || true)"
  case "$initrd_us$userspace_us" in ''|*[!0-9]*) return 0 ;; esac
  [ "$initrd_us" -gt 0 ] && [ "$userspace_us" -gt "$initrd_us" ] || return 0
  log="$(journalctl -b -o short-monotonic --no-pager 2>/dev/null)" || return 0
  [ -n "$log" ] || return 0
  printf 'initrd, longest gaps before switch-root:\n'
  awk -F'[][]' -v start="$initrd_us" -v end="$userspace_us" '
    { t = $2 * 1e6 }
    t < start { next }
    t > end { exit }
    p != "" && t - p > 300000 { m = $0; sub(/^[^]]*]/, "", m); printf "  %7.3fs before [%s]%s\n", (t - p) / 1e6, $2, substr(m, 1, 110) }
    { p = t }' <<<"$log" | sort -rn | head -10 || true
  # A busy initrd has no gaps: name the units that ran longest instead, from
  # systemd's own Starting/Finished lines in the same window.
  printf 'initrd, longest units (%s journal entries in the phase):\n' \
    "$(awk -F'[][]' -v start="$initrd_us" -v end="$userspace_us" \
         '{ t = $2 * 1e6 } t >= start && t <= end { n++ } END { print n + 0 }' <<<"$log")"
  awk -F'[][]' -v start="$initrd_us" -v end="$userspace_us" '
    { t = $2 * 1e6 }
    t < start || t > end { next }
    {
      m = $0; sub(/^[^]]*]/, "", m)
      if (m ~ /systemd\[1\]: Starting /) {
        u = m; sub(/.*systemd\[1\]: Starting /, "", u); sub(/ - .*/, "", u); sub(/\.\.\.$/, "", u)
        begun[u] = t
      } else if (m ~ /systemd\[1\]: (Finished|Started) /) {
        u = m; sub(/.*systemd\[1\]: (Finished|Started) /, "", u); sub(/ - .*/, "", u); sub(/\.$/, "", u)
        if (u in begun) { printf "  %7.3fs %s\n", (t - begun[u]) / 1e6, u; delete begun[u] }
      }
    }
    END { for (u in begun) printf "  %7.3fs %s (still running at switch-root)\n", (end - begun[u]) / 1e6, u }' \
    <<<"$log" | sort -rn | head -8 || true
}
# The pivot into the real root logs no unit in between; systemd's own
# milestones across it say where its time went (policy load, relabel, the
# new manager's first job).
switch_root_steps() {
  local log
  log="$(journalctl -b -o short-monotonic --no-pager 2>/dev/null)" || return 0
  printf 'switch-root milestones:\n'
  grep -E 'Switching root|SELinux policy|Relabeled|systemd [0-9]+[.0-9-]* running|Queued start job for default target|initrd-switch-root.service: Deactivated' <<<"$log" \
    | cut -c1-160 | sed 's/^/  /' | head -12 || true
}

# bootc removes /etc/.updated from every deployment, so its first boot runs the
# ConditionNeedsUpdate= units. The caches the image builds (the linker cache in
# /etc, the hardware database in /usr) must not be among them.
image_cache_units=(ldconfig.service systemd-hwdb-update.service)
rebuilt_image_caches() {
  local unit
  for unit in "${image_cache_units[@]}"; do
    [ "$(systemctl show "$unit" --property=ConditionResult --value 2>/dev/null || true)" != yes ] \
      || printf '%s\n' "$unit"
  done
}

first_boot_update_check() {
  local result path
  result="$(systemctl show ldconfig.service --property=ConditionResult --value 2>/dev/null || true)"
  printf 'first-boot update check: ldconfig condition %s\n' "${result:-unknown}"
  # The booted view, the mount behind /usr, and the deployment's own /etc
  # (bootc's merged copy), to tell which side moved.
  for path in /usr /etc/.updated /etc/ld.so.cache /sysroot/state/deploy/*/etc/.updated \
      /sysroot/ostree/deploy/*/deploy/*/etc/.updated; do
    [ -e "$path" ] || continue
    printf '  %s %s\n' "$(stat -c '%y' "$path" 2>/dev/null)" "$path"
  done
  printf '  /usr is %s\n' "$(findmnt -no SOURCE,FSTYPE --target /usr 2>/dev/null | head -1)"
}

kernel_gaps() {
  local log
  # Journald timestamps imported early messages at receipt. Those timestamps
  # can place every early message after the initrd started. Read the kernel's
  # own clock instead; an unprivileged failure must not fabricate kernel gaps.
  # dmesg is restricted for the gate user. journald keeps the kernel's own
  # timestamp of each kernel message as _SOURCE_MONOTONIC_TIMESTAMP, which the
  # gate user reads through wheel.
  log="$(dmesg --color=never 2>/dev/null)" || log="$(sudo -n dmesg --color=never 2>/dev/null)" \
    || log="$(journalctl -k -b -o json --no-pager 2>/dev/null | python3 -c '
import json, sys
for line in sys.stdin:
    entry = json.loads(line)
    stamp = entry.get("_SOURCE_MONOTONIC_TIMESTAMP")
    message = entry.get("MESSAGE")
    if stamp and isinstance(message, str):
        print(f"[{int(stamp) / 1e6:12.6f}] {message}")
' 2>/dev/null)" && [ -n "$log" ] || {
    printf 'raw kernel timestamps unavailable; no kernel gap attribution\n'
    return 0
  }
  [ -n "$log" ] || return 0
  # Only a line that starts with the kernel's [seconds] stamp carries a time.
  # A continuation line (the rest of a multi-line message) has none: reading it
  # as 0 would invent a gap as long as the whole boot up to the next stamp.
  awk -F'[][]' '/^\[ *[0-9]+\.[0-9]+\]/ { printf "kernel log starts at %.3fs\n", $2 + 0; exit }' <<<"$log"
  printf 'kernel log, longest gaps before the initrd:\n'
  awk -F'[][]' -v end="${initrd_us_for_gaps:-0}" '
    !/^\[ *[0-9]+\.[0-9]+\]/ { next }
    { t = $2 + 0 }
    end > 0 && t * 1e6 > end { exit }
    p != "" && t - p > 0.2 { printf "  %7.3fs before [%s]%s\n", t - p, $2, substr($3, 1, 110) }
    { p = t }' <<<"$log" | sort -rn | head -12 || true
  grep -E 'Trying to unpack|Freeing initrd memory|Run /init' <<<"$log" | cut -c1-140 || true
}
initrd_us_for_gaps="$(systemctl show --property=InitRDTimestampMonotonic --value 2>/dev/null || true)"
case "$initrd_us_for_gaps" in ''|*[!0-9]*) initrd_us_for_gaps=0 ;; esac
# The guest's CPU time stolen by its host since boot. A slow phase with high
# steal is host contention (another VM or build on the same host), not the
# image; with near-zero steal the time is the guest's own.
cpu_steal() {
  local stat="${TILO_PROC_STAT:-/proc/stat}" hz
  [ -r "$stat" ] || return 0
  hz="$(getconf CLK_TCK 2>/dev/null || echo 100)"
  awk -v hz="$hz" -v up_us="$entered_us" '
    $1 == "cpu" { steal = $9 }
    $1 ~ /^cpu[0-9]+$/ { cpus++ }
    END {
      if (steal == "" || cpus == 0) exit
      s = steal / hz
      printf "cpu steal since boot: %.3fs over %d CPUs (%.0f%% of %.3fs)\n",
        s, cpus, 100 * s / (cpus * up_us / 1e6), up_us / 1e6 }' "$stat"
}
# The time the boot spent waiting on its host, not on the image: its disk reads
# queued behind other VMs and builds on the same disk (the kernel's I/O
# pressure, "full": every runnable task of the guest waiting on I/O), and its
# vCPUs not run (steal, averaged over the CPUs). Kernel time, which reads no
# disk, stays the same on a loaded host while every phase that reads the disk
# grows, so it is the host's and not part of the budget. TILO_BOOT_HOST_SAMPLES
# names samples the guest took of both while it booted, one a line: uptime in
# seconds, the I/O "full" total in microseconds, steal ticks of /proc/stat. The
# wait at the greeter is that of the last sample before it, or of the first after
# it less the time since (a wait grows no faster than the clock), so it is never
# counted larger than it was. Without samples the whole boot counts.
host_wait_us=0
host_wait() {
  local samples="${TILO_BOOT_HOST_SAMPLES:-}" stat="${TILO_PROC_STAT:-/proc/stat}" hz cpus
  [ -n "$samples" ] && [ -s "$samples" ] || return 0
  hz="$(getconf CLK_TCK 2>/dev/null || echo 100)"
  cpus="$(awk '$1 ~ /^cpu[0-9]+$/ { n++ } END { print n + 0 }' "$stat" 2>/dev/null || echo 0)"
  [ "$cpus" -gt 0 ] || return 0
  awk -v e="$entered_us" -v hz="$hz" -v cpus="$cpus" '
    NF < 3 || $1 !~ /^[0-9.]+$/ || $2 !~ /^[0-9]+$/ || $3 !~ /^[0-9]+$/ { next }
    { t = $1 * 1e6; w = $2 + $3 * 1e6 / hz / cpus }
    t <= e { best = w; found = 1; next }
    !found { best = w - (t - e); found = 1; exit }
    END { if (best < 0) best = 0; printf "%d\n", best }' "$samples"
}
host_wait_us="$(host_wait)"
case "$host_wait_us" in ''|*[!0-9]*) host_wait_us=0 ;; esac
judged_us=$((entered_us - host_wait_us))
host_wait_note() {
  [ "$host_wait_us" -gt 0 ] || return 0
  awk -v w="$host_wait_us" -v j="$judged_us" 'BEGIN {
    printf "of it %.3fs waiting on the host (I/O pressure and CPU steal), %.3fs the image'"'"'s\n", w / 1e6, j / 1e6 }'
}
# Within half the budget the total is enough; a closer or failing boot also
# names where its time went.
if [ "$judged_us" -gt $((budget_us / 2)) ] && [ "$judged_us" -le "$budget_us" ]; then
  cpu_steal
  kernel_gaps
  initrd_gaps
  switch_root_steps
  first_boot_update_check
  systemd-analyze critical-chain graphical.target 2>/dev/null | head -14 || true
fi
if [ "$judged_us" -gt "$budget_us" ]; then
  printf 'kernel+userspace reached usable greeter in %d.%03ds (budget: %ss)\n' \
    "$seconds" "$milliseconds" "$budget_seconds" >&2
  host_wait_note >&2
  boot_phases >&2
  cpu_steal >&2
  kernel_gaps >&2
  initrd_gaps >&2
  switch_root_steps >&2
  first_boot_update_check >&2
  systemd-analyze critical-chain graphical.target >&2 || true
  systemd-analyze blame --no-pager 2>/dev/null | grep -v '\.device$' | head -20 >&2 || true
  exit 1
fi
boot_phases
printf 'kernel+userspace reached usable greeter in %d.%03ds (budget: %ss)\n' \
  "$seconds" "$milliseconds" "$budget_seconds"
host_wait_note
rebuilt="$(rebuilt_image_caches)"
if [ -n "$rebuilt" ]; then
  printf 'this boot rebuilt caches the image already carries: %s\n' "$(tr '\n' ' ' <<<"$rebuilt")" >&2
  first_boot_update_check >&2
  exit 1
fi
