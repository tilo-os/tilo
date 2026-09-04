#!/usr/bin/env bash
# Prove the bootloader swap took: verify the produced desktop qcow2 has
# systemd-boot on its ESP, boots on UEFI/OVMF, and reaches multi-user.target.
#
# The disk-image step invokes the same install code path bootc uses at
# install-time (bootc install to-filesystem), reading the drop-in at
# /usr/lib/bootc/install/*.toml baked into the image. If bib produced a
# qcow2 whose ESP holds systemd-bootx64.efi + loader.conf (and NOT
# grubx64.efi), the swap is live for every install path that shares that
# code path — including the custom installer (`qos-install` -> `bootc install
# to-disk`) and the Anaconda `ostreecontainer` install path.
#
# Fully non-interactive. No window pops open; nothing waits for a keypress.
# Emits a machine-readable JSON summary on stdout for CI + a human-readable
# log to $OUT_DIR/verify.log. Exit codes:
#   0  systemd-boot verified on the ESP + guest reached multi-user.target
#   2  usage (bad arg or missing input)
#   3  precondition failed (no qcow2 to test)
#   6  ESP layout is not systemd-boot's
#   7  guest failed to reach the multi-user/graphical target
#   8  qemu did not start
#
# Usage:
#   scripts/perf/qemu-install-boot-verify.sh [--disk PATH]
#
# Environment overrides (all optional):
#   DISK           path to the qcow2 to verify
#                  (default: os/output/qcow2/disk.qcow2)
#   OUT_DIR        where to leave logs + JSON
#                  (default: target/perf/systemd-boot-verify)
#   BOOT_TIMEOUT   how long to wait for multi-user.target, seconds
#                  (default: 240)
#
# The verify does NOT modify the input qcow2 — the boot runs against a
# throwaway snapshot backed by the input, so re-runs are hermetic.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

DISK="${DISK:-$REPO_ROOT/os/output/qcow2/disk.qcow2}"
OUT_DIR="${OUT_DIR:-$REPO_ROOT/target/perf/systemd-boot-verify}"
BOOT_TIMEOUT="${BOOT_TIMEOUT:-240}"

while [ $# -gt 0 ]; do
  case "$1" in
    --disk) DISK="$2"; shift 2 ;;
    --out) OUT_DIR="$2"; shift 2 ;;
    --timeout) BOOT_TIMEOUT="$2"; shift 2 ;;
    -h|--help) sed -n '2,32p' "$0"; exit 0 ;;
    *) printf 'unknown arg: %s\n' "$1" >&2; exit 2 ;;
  esac
done

# The env where qemu-system-x86_64, qemu-img, OVMF and socat live. On the
# reference machine that's a Fedora distrobox; on a host with those tools
# native, set QOS_DISTROBOX="" (see os/dev/_common.sh). The ESP inspection
# needs mtools (mattrib + mdir) on the host — that's the only requirement
# outside the distrobox.
# shellcheck disable=SC1091
source "$REPO_ROOT/os/dev/_common.sh"

SNAP="$OUT_DIR/snapshot.qcow2"
VARS="$OUT_DIR/vars.fd"
CONSOLE="$OUT_DIR/boot.console.log"
# AF_UNIX socket paths are capped at 108 bytes; a deep $OUT_DIR overflows
# that on the qemu -monitor arg. Keep the monitor socket under a short
# fixed prefix so the -monitor arg stays valid regardless of $OUT_DIR.
SHORT_RUN="${SHORT_RUN:-/tmp/qos-vb-$$}"
MON="$SHORT_RUN/mon.sock"
PID="$OUT_DIR/qemu.pid"
LOG="$OUT_DIR/verify.log"
JSON="$OUT_DIR/result.json"

mkdir -p "$OUT_DIR" "$SHORT_RUN"
: > "$LOG"
SB_PRESENT=null
GRUB_PRESENT=null
LOADER_CONF_PRESENT=null
BOOT_MS=null
BOOT_SIGNAL=""

log() { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*" | tee -a "$LOG"; }

cleanup() {
  qemu_pid="$(cat "$PID" 2>/dev/null || true)"
  if [[ "$qemu_pid" =~ ^[0-9]+$ ]]; then
    inenv "kill '$qemu_pid' 2>/dev/null || true"
    sleep 1
    inenv "kill -9 '$qemu_pid' 2>/dev/null || true"
    rm -f "$PID"
  fi
  rm -f "$MON" "$SNAP" "$OUT_DIR/full.img" "$OUT_DIR/esp.img"
  rmdir "$SHORT_RUN" 2>/dev/null || true
}
trap cleanup EXIT

emit_json() {
  # $1 = phase, $2 = ok (true/false), $3 = detail
  cat > "$JSON" <<EOF
{
  "phase": "$1",
  "ok": $2,
  "detail": "$3",
  "systemd_boot_efi_present": $SB_PRESENT,
  "grub_efi_present": $GRUB_PRESENT,
  "loader_conf_present": $LOADER_CONF_PRESENT,
  "boot_signal": "$BOOT_SIGNAL",
  "boot_ms": $BOOT_MS,
  "log": "$LOG",
  "disk": "$DISK"
}
EOF
  cat "$JSON"
}

# --- Preconditions ----------------------------------------------------------
[ -f "$DISK" ] || {
  log "no qcow2 at $DISK — build one first: ./os/dev/build-image.sh"
  emit_json "precondition" false "missing qcow2"; exit 3
}

firmware_setup
inenv "cp -f '$QOS_FW_VARS' '$VARS'"

# Backing-file snapshot so the input qcow2 stays byte-for-byte identical.
# Same trick scripts/perf/qemu-boot-measure.sh uses.
inenv "qemu-img create -f qcow2 -F qcow2 -b '$DISK' '$SNAP'" >/dev/null

# --- Phase 1: inspect the ESP (offline) ------------------------------------
# Extract the ESP partition to a plain raw file and read it with mtools
# (which understands FAT-32 directly). This avoids qemu-nbd / kernel NBD /
# loop mounts / privileged mount points — a rootless read of the ESP works
# on the reference workstation, in a distrobox, and in CI without any
# extra kernel modules. `qemu-img map` prints the partition ranges from
# the qcow2, `qemu-img dd` extracts one to a raw file.
log "phase 1: extracting ESP from qcow2 to read with mtools"
ESP_RAW="$OUT_DIR/esp.img"
rm -f "$ESP_RAW"

# The ESP is the FAT-32 partition. `qemu-img map --output=json` gives every
# extent, but not partition boundaries; `qemu-img info` doesn't either. Do
# it two-step: `qemu-img convert` to a temporary raw image (sparse, so the
# cost is only allocated data), then `sfdisk -J` for the partition table.
FULL_RAW="$OUT_DIR/full.img"
rm -f "$FULL_RAW"
inenv "qemu-img convert -O raw '$DISK' '$FULL_RAW'"

# Find the ESP: EFI System partition GUID or plain FAT-32 by type.
ESP_INFO="$(inenv "sfdisk -J '$FULL_RAW'" | python3 -c "
import json, sys
p = json.load(sys.stdin)
for part in p['partitiontable']['partitions']:
    t = part.get('type', '').lower()
    # EFI System Partition GUID (GPT) or type 0xEF (MBR).
    if t == 'c12a7328-f81f-11d2-ba4b-00a0c93ec93b' or t == 'ef':
        # sfdisk emits start/size in 512-byte sectors.
        print('%d %d' % (part['start']*512, part['size']*512))
        sys.exit(0)
sys.exit(1)
")" || {
  emit_json "inspect" false "no ESP (EFI System Partition) in the disk's partition table"
  rm -f "$FULL_RAW"
  exit 6
}
ESP_OFFSET="${ESP_INFO%% *}"
ESP_SIZE="${ESP_INFO##* }"
log "phase 1: ESP at offset=$ESP_OFFSET size=$ESP_SIZE bytes"

# Slice the ESP out; keeps mtools invocations simple (no --offset math).
dd if="$FULL_RAW" of="$ESP_RAW" bs=512 skip=$((ESP_OFFSET/512)) count=$((ESP_SIZE/512)) status=none
rm -f "$FULL_RAW"

# Layout signals per bootloader:
#   * systemd-boot: EFI/systemd/systemd-bootx64.efi + loader/loader.conf
#     (BOOTX64.EFI is the removable-media fallback and may be either).
#   * GRUB:         EFI/fedora/grubx64.efi + EFI/fedora/grub.cfg + shim
# Assert the systemd-boot pair is present AND the GRUB pair is not — the
# strongest one-shot signal for the swap. `mdir -i FILE ::PATH` walks the
# FAT tree directly; mtype (or a `mattrib` existence check) verifies files.
SB_PRESENT=false
GRUB_PRESENT=false
LOADER_CONF_PRESENT=false
mtools_has() {
  # $1 = image, $2 = /-rooted path. Non-zero if not found.
  MTOOLS_SKIP_CHECK=1 mattrib -i "$1" "::$2" >/dev/null 2>&1
}
mtools_has "$ESP_RAW" "EFI/systemd/systemd-bootx64.efi" && SB_PRESENT=true
mtools_has "$ESP_RAW" "loader/loader.conf"              && LOADER_CONF_PRESENT=true
if mtools_has "$ESP_RAW" "EFI/fedora/grubx64.efi" || mtools_has "$ESP_RAW" "EFI/fedora/grub.cfg"; then
  GRUB_PRESENT=true
fi

log "phase 1: ESP tree:"
MTOOLS_SKIP_CHECK=1 mdir -i "$ESP_RAW" -/ ::/EFI 2>&1 | tee -a "$LOG" || true
log "phase 1: systemd-boot=$SB_PRESENT loader.conf=$LOADER_CONF_PRESENT grub=$GRUB_PRESENT"

rm -f "$ESP_RAW"

if [ "$SB_PRESENT" != "true" ] || [ "$LOADER_CONF_PRESENT" != "true" ] || [ "$GRUB_PRESENT" = "true" ]; then
  emit_json "inspect" false "ESP layout does not match systemd-boot"; exit 6
fi

# --- Phase 2: boot the snapshot and measure --------------------------------
log "phase 2: boot snapshot=$SNAP under OVMF, wait for multi-user.target"
BOOT_START="$(date +%s)"
rm -f "$MON" "$CONSOLE"

BOOT_ARGS="\
  $(machine_args) $(secboot_qemu_args) -enable-kvm -m 4096 -cpu host -smp 2 \
  $(pflash_args "$VARS") \
  -drive file=$SNAP,format=qcow2,if=virtio \
  -device virtio-vga -display none \
  -nic user,model=virtio-net-pci \
  -monitor unix:$MON,server,nowait \
  -serial file:$CONSOLE \
  -pidfile $PID -name qos-systemd-boot-verify"

inenv "setsid nohup qemu-system-x86_64 $BOOT_ARGS >/dev/null 2>&1 & disown"
sleep 2
inenv "test -S '$MON'" || { emit_json "boot" false "qemu did not start"; exit 8; }

DEADLINE=$(( BOOT_START + BOOT_TIMEOUT ))
BOOT_STATE="unknown"
BOOT_SIGNAL=""
while [ "$(date +%s)" -lt "$DEADLINE" ]; do
  # Accept a bare or ANSI-coloured systemd target milestone.
  if [ -f "$CONSOLE" ] && sed 's/\x1b\[[0-9;]*[mGKH]//g' "$CONSOLE" 2>/dev/null | \
      grep -qE "Reached target [^-]*(multi-user\.target|graphical\.target)"; then
    BOOT_STATE="reached"; BOOT_SIGNAL="systemd-target"; break
  fi
  # Release images do not necessarily mirror systemd target milestones to the
  # serial journal. A real getty login prompt is an equally strong downstream
  # signal: systemd cannot start it before the boot reaches its configured
  # multi-user target.
  if [ -f "$CONSOLE" ] && tr -d '\000\r' < "$CONSOLE" 2>/dev/null \
      | grep -qE '(^|[[:space:]])[^[:space:]]+[[:space:]]+login:[[:space:]]*$'; then
    BOOT_STATE="reached"; BOOT_SIGNAL="serial-getty"; break
  fi
  if grep -qE "Kernel panic|Failed to load kernel|Boot failed" "$CONSOLE" 2>/dev/null; then
    BOOT_STATE="panic"; break
  fi
  if ! inenv "kill -0 \$(cat '$PID') 2>/dev/null"; then
    BOOT_STATE="qemu-exited"; break
  fi
  sleep 2
done

# Same measurement as scripts/perf/qemu-boot-measure.sh — the kernel-log
# timestamp on the `Reached target multi-user.target` line is the
# wall-clock since boot in seconds. Reused verbatim so the numbers are
# apples-to-apples with the prior GRUB baseline.
if [ "$BOOT_SIGNAL" = "systemd-target" ]; then
  BOOT_MS="$(sed 's/\x1b\[[0-9;]*[mGKH]//g' "$CONSOLE" 2>/dev/null | \
    awk '
      match($0, /\[[[:space:]]*([0-9]+\.[0-9]+)\]/, m) { last_ts=m[1] }
      /Reached target [^-]*(multi-user\.target|graphical\.target)/ {
        printf "%d", last_ts * 1000
        exit
      }
    ' || true)"
  [ -n "$BOOT_MS" ] || BOOT_MS=$(( ($(date +%s) - BOOT_START) * 1000 ))
else
  # Getty has no kernel timestamp on its prompt, so report honest wall-clock
  # elapsed time rather than manufacturing a zero-duration boot.
  BOOT_MS=$(( ($(date +%s) - BOOT_START) * 1000 ))
fi

# Look for the systemd-boot marker in the serial log ("Loading Boot Loader
# Menu" won't appear on a single-entry silent boot, but the systemd-boot
# kernel-cmdline plumbing sets `systemd.boot-loader-entry` / prints the
# entry title on the console; also, grub's own `GNU GRUB` banner MUST NOT
# be there). Log for humans; do not use as the pass/fail gate — the on-disk
# check is the source of truth.
if grep -qi "GNU GRUB" "$CONSOLE" 2>/dev/null; then
  log "warn: serial console contains 'GNU GRUB' — swap did not take"
fi

log "phase 2: state=$BOOT_STATE signal=$BOOT_SIGNAL boot_ms=$BOOT_MS"

# ACPI-shutdown so the run leaves no orphan process behind.
qemu_pid="$(cat "$PID" 2>/dev/null || true)"
if [[ "$qemu_pid" =~ ^[0-9]+$ ]] && inenv "kill -0 '$qemu_pid' 2>/dev/null"; then
  inenv "printf 'system_powerdown\n' | socat - 'unix:$MON'" >/dev/null 2>&1 || true
  for _ in $(seq 1 30); do
    inenv "kill -0 '$qemu_pid' 2>/dev/null" || break
    sleep 1
  done
  inenv "kill -9 '$qemu_pid' 2>/dev/null || true"
fi
rm -f "$PID"

if [ "$BOOT_STATE" = "reached" ]; then
  emit_json "verified" true "systemd-boot on ESP + boot reached $BOOT_SIGNAL in ${BOOT_MS}ms"
  exit 0
fi

emit_json "boot" false "$BOOT_STATE"
exit 7
