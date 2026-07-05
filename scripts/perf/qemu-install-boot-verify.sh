#!/usr/bin/env bash
# Prove the bootloader swap took: verify the produced desktop qcow2 has
# systemd-boot on its ESP, boots on UEFI/OVMF, and reaches multi-user.target.
#
# bootc-image-builder invokes the same install code path bootc uses at
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

# The env where qemu, OVMF, qemu-img, socat and qemu-nbd all live. On the
# reference machine that's a Fedora distrobox; on a host with those tools
# native, set QOS_DISTROBOX="" (see os/dev/_common.sh).
# shellcheck disable=SC1091
source "$REPO_ROOT/os/dev/_common.sh"

SNAP="$OUT_DIR/snapshot.qcow2"
VARS="$OUT_DIR/vars.fd"
CONSOLE="$OUT_DIR/boot.console.log"
MON="$OUT_DIR/mon.sock"
PID="$OUT_DIR/qemu.pid"
ESP_MNT="$OUT_DIR/mnt.esp"
LOG="$OUT_DIR/verify.log"
JSON="$OUT_DIR/result.json"

mkdir -p "$OUT_DIR"
: > "$LOG"
SB_PRESENT=null
GRUB_PRESENT=null
LOADER_CONF_PRESENT=null
BOOT_MS=null

log() { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*" | tee -a "$LOG"; }

cleanup() {
  if [ -f "$PID" ]; then
    inenv "kill \$(cat '$PID') 2>/dev/null || true"
    sleep 1
    inenv "kill -9 \$(cat '$PID') 2>/dev/null || true"
    rm -f "$PID"
  fi
  if mountpoint -q "$ESP_MNT" 2>/dev/null; then
    sudo umount "$ESP_MNT" 2>/dev/null || true
  fi
  sudo qemu-nbd -d /dev/nbd0 2>/dev/null || true
  rm -f "$MON" "$SNAP"
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

CODE="$(ovmf_code)"
VARS_SRC="$(ovmf_vars)"
inenv "cp -f '$VARS_SRC' '$VARS'"

# Backing-file snapshot so the input qcow2 stays byte-for-byte identical.
# Same trick scripts/perf/qemu-boot-measure.sh uses.
inenv "qemu-img create -f qcow2 -F qcow2 -b '$DISK' '$SNAP'" >/dev/null

# --- Phase 1: inspect the ESP (offline) ------------------------------------
# Attaching read-only via qemu-nbd is the source of truth. Watching the
# firmware boot menu on a serial console is fragile (systemd-boot with a
# single entry is quiet); the on-disk layout is not.
log "phase 1: mount input qcow2 read-only to inspect ESP"
sudo modprobe nbd max_part=8 2>/dev/null || true
sudo qemu-nbd -d /dev/nbd0 2>/dev/null || true
sudo qemu-nbd --read-only --connect=/dev/nbd0 "$DISK"
sudo partprobe /dev/nbd0 2>/dev/null || true
sleep 1

ESP_PART=""
for p in /dev/nbd0p1 /dev/nbd0p2 /dev/nbd0p3; do
  [ -b "$p" ] || continue
  t="$(sudo blkid -o value -s TYPE "$p" 2>/dev/null || true)"
  if [ "$t" = "vfat" ]; then ESP_PART="$p"; break; fi
done
if [ -z "$ESP_PART" ]; then
  sudo qemu-nbd -d /dev/nbd0 || true
  emit_json "inspect" false "no ESP (vfat partition) on disk"
  exit 6
fi

mkdir -p "$ESP_MNT"
sudo mount -o ro "$ESP_PART" "$ESP_MNT"

# Layout signals for each bootloader:
#   * systemd-boot: EFI/systemd/systemd-bootx64.efi + loader/loader.conf
#     (BOOTX64.EFI is the removable-media fallback and may be either).
#   * GRUB:         EFI/fedora/grubx64.efi + EFI/fedora/grub.cfg + shim
# Assert the systemd-boot pair is present AND the GRUB pair is not — the
# strongest one-shot signal for the swap.
SB_PRESENT=false
GRUB_PRESENT=false
LOADER_CONF_PRESENT=false
if sudo test -f "$ESP_MNT/EFI/systemd/systemd-bootx64.efi"; then
  SB_PRESENT=true
fi
if sudo test -f "$ESP_MNT/loader/loader.conf"; then
  LOADER_CONF_PRESENT=true
fi
if sudo test -f "$ESP_MNT/EFI/fedora/grubx64.efi" || \
   sudo test -f "$ESP_MNT/EFI/fedora/grub.cfg"; then
  GRUB_PRESENT=true
fi
log "phase 1: ESP tree (top 3 levels):"
sudo find "$ESP_MNT" -maxdepth 3 2>/dev/null | tee -a "$LOG" || true
log "phase 1: systemd-boot=$SB_PRESENT loader.conf=$LOADER_CONF_PRESENT grub=$GRUB_PRESENT"

sudo umount "$ESP_MNT"
sudo qemu-nbd -d /dev/nbd0 || true

if [ "$SB_PRESENT" != "true" ] || [ "$LOADER_CONF_PRESENT" != "true" ] || [ "$GRUB_PRESENT" = "true" ]; then
  emit_json "inspect" false "ESP layout does not match systemd-boot"; exit 6
fi

# --- Phase 2: boot the snapshot and measure --------------------------------
log "phase 2: boot snapshot=$SNAP under OVMF, wait for multi-user.target"
BOOT_START="$(date +%s)"
rm -f "$MON" "$CONSOLE"

BOOT_ARGS="\
  -machine q35,accel=kvm -enable-kvm -m 4096 -cpu host -smp 2 \
  -drive if=pflash,format=raw,readonly=on,file=$CODE \
  -drive if=pflash,format=raw,file=$VARS \
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
while [ "$(date +%s)" -lt "$DEADLINE" ]; do
  # Two accepted signals:
  #   * bare `Reached target ... multi-user.target` from journald->console,
  #   * ANSI-coloured `[  OK  ] Reached target ...` (strip escape sequences).
  if [ -f "$CONSOLE" ] && sed 's/\x1b\[[0-9;]*[mGKH]//g' "$CONSOLE" 2>/dev/null | \
      grep -qE "Reached target [^-]*(multi-user\.target|graphical\.target)"; then
    BOOT_STATE="reached"; break
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
BOOT_MS="$(sed 's/\x1b\[[0-9;]*[mGKH]//g' "$CONSOLE" 2>/dev/null | \
  awk '
    match($0, /\[[[:space:]]*([0-9]+\.[0-9]+)\]/, m) { last_ts=m[1] }
    /Reached target [^-]*(multi-user\.target|graphical\.target)/ {
      printf "%d", last_ts * 1000
      exit
    }
  ' || echo 0)"
[ -n "$BOOT_MS" ] || BOOT_MS=0

# Look for the systemd-boot marker in the serial log ("Loading Boot Loader
# Menu" won't appear on a single-entry silent boot, but the systemd-boot
# kernel-cmdline plumbing sets `systemd.boot-loader-entry` / prints the
# entry title on the console; also, grub's own `GNU GRUB` banner MUST NOT
# be there). Log for humans; do not use as the pass/fail gate — the on-disk
# check is the source of truth.
if grep -qi "GNU GRUB" "$CONSOLE" 2>/dev/null; then
  log "warn: serial console contains 'GNU GRUB' — swap did not take"
fi

log "phase 2: state=$BOOT_STATE boot_ms=$BOOT_MS"

# ACPI-shutdown so the run leaves no orphan process behind.
if inenv "kill -0 \$(cat '$PID') 2>/dev/null"; then
  inenv "printf 'system_powerdown\n' | socat - 'unix:$MON'" >/dev/null 2>&1 || true
  for _ in $(seq 1 30); do
    inenv "kill -0 \$(cat '$PID') 2>/dev/null" || break
    sleep 1
  done
  inenv "kill -9 \$(cat '$PID') 2>/dev/null || true"
fi
rm -f "$PID"

if [ "$BOOT_STATE" = "reached" ]; then
  emit_json "verified" true "systemd-boot on ESP + boot completed in ${BOOT_MS}ms"
  exit 0
fi

emit_json "boot" false "$BOOT_STATE"
exit 7
