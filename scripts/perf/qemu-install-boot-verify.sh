#!/usr/bin/env bash
# Prove the bootloader swap took: verify the produced desktop qcow2 has
# systemd-boot on its ESP, boots on UEFI/OVMF, and reaches multi-user.target.
#
# The disk-image step invokes the same install code path bootc uses at
# install-time (bootc install to-filesystem), reading the drop-in at
# /usr/lib/bootc/install/*.toml baked into the image. If bib produced a
# qcow2 whose ESP holds systemd-bootx64.efi + loader.conf (and NOT
# grubx64.efi), the swap is live for every install path that shares that
# code path — including the custom installer (`tilo-install` -> `bootc install
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
# native, set TILO_DISTROBOX="" (see os/dev/_common.sh). The ESP inspection
# needs mtools (mattrib + mdir) on the host — that's the only requirement
# outside the distrobox.
# shellcheck disable=SC1091
source "$REPO_ROOT/os/dev/_common.sh"

CONSOLE="$OUT_DIR/boot.console.log"
# The VM's runtime directory holds its monitor socket, and AF_UNIX socket
# paths are capped at 108 bytes; a deep $OUT_DIR overflows that. Keep it
# under a short fixed prefix regardless of $OUT_DIR.
SHORT_RUN="${SHORT_RUN:-/tmp/tilo-vb-$$}"
LOG="$OUT_DIR/verify.log"
JSON="$OUT_DIR/result.json"
# What `tilo-vm qemu run` measured: its state, the line it matched, and the
# boot time (the kernel's own clock at that line, else wall clock).
BOOT_JSON="$OUT_DIR/boot.json"

mkdir -p "$OUT_DIR" "$SHORT_RUN"
: > "$LOG"
SB_PRESENT=null
GRUB_PRESENT=null
LOADER_CONF_PRESENT=null
BOOT_MS=null
BOOT_SIGNAL=""

log() { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*" | tee -a "$LOG"; }

cleanup() {
  tilo_vm qemu stop --run "$SHORT_RUN" >/dev/null 2>&1 || true
  rm -f "$OUT_DIR/full.img" "$OUT_DIR/esp.img"
  rm -rf "$SHORT_RUN"
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
# --- Phase 2: boot the disk and measure ------------------------------------
# The input qcow2 is never modified: `qemu run --overlay` boots a throwaway
# child of it, and the run powers the guest off when it is done.
log "phase 2: boot $DISK under OVMF, wait for multi-user.target or a login prompt"
rm -f "$CONSOLE" "$BOOT_JSON"

# Release images do not necessarily mirror systemd target milestones to the
# serial journal, so a getty's login prompt is an equally strong downstream
# signal: systemd cannot start it before the boot reached its configured
# multi-user target. The kernel's timestamp on whichever line matched is the
# boot time, in the same terms `qemu run --kernel-time` reports.
TARGET='Reached target [^-]*(multi-user\.target|graphical\.target)'
GETTY='(^|[[:space:]])[^[:space:]]+[[:space:]]+login:[[:space:]]*$'
boot_ok=true
tilo_vm qemu run --run "$SHORT_RUN" --name tilo-systemd-boot-verify \
  --disk "$DISK" --overlay --accel kvm --memory 4096 --smp 2 \
  --display 2d --input none --serial-log "$CONSOLE" --timeout "$BOOT_TIMEOUT" \
  --ready "$TARGET" --ready "$GETTY" \
  --fail 'Kernel panic|Failed to load kernel|Boot failed' \
  --report "$BOOT_JSON" >>"$LOG" 2>&1 || boot_ok=false

BOOT_STATE=unknown
BOOT_SIGNAL=none
if [ -s "$BOOT_JSON" ]; then
  read -r BOOT_STATE BOOT_MS BOOT_SIGNAL <<<"$(python3 - "$BOOT_JSON" <<'PY'
import json, sys
report = json.load(open(sys.argv[1]))
pattern = report.get("pattern") or ""
signal = "systemd-target" if "target" in pattern else "serial-getty" if pattern else "none"
timing = report.get("kernel_ms") or report.get("elapsed_ms") or 0
print(report.get("state", "unknown"), int(timing), signal)
PY
)"
  [ "$BOOT_STATE" = ready ] && BOOT_STATE=reached
else
  emit_json "boot" false "qemu did not start"; exit 8
fi
[ "$boot_ok" = true ] || log "phase 2: the boot did not pass; see $LOG"

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

if [ "$BOOT_STATE" = reached ]; then
  emit_json "verified" true "systemd-boot on ESP + boot reached $BOOT_SIGNAL in ${BOOT_MS}ms"
  exit 0
fi

emit_json "boot" false "$BOOT_STATE"
exit 7
