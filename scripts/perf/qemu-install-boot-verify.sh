#!/usr/bin/env bash
# End-to-end QEMU verification that the bootloader swap is real: install the
# qOS desktop image onto a blank disk (via the custom-installer live medium),
# reboot the installed disk, confirm the bootloader on the ESP is systemd-boot
# (not GRUB), and measure the wall-clock from the bootloader's first output to
# `Reached target multi-user.target`.
#
# Fully non-interactive. No qemu window pops open; nothing waits on human input.
# Emits a machine-readable JSON summary on stdout for CI + a human-readable log
# to $OUT_DIR/verify.log. Exit code: 0 = systemd-boot verified + boot completed
# under the timeout; 2 = usage; anything else = failure (see the log).
#
#   scripts/perf/qemu-install-boot-verify.sh
#
# Environment overrides (all optional):
#   INSTALLER_DISK   path to the custom-installer live qcow2
#                    (default: os/output/installer/qcow2/disk.qcow2)
#   TARGET_SIZE      size of the fresh target disk this creates
#                    (default: 20G)
#   OUT_DIR          where to leave logs + JSON
#                    (default: target/perf/systemd-boot-verify)
#   INSTALL_TIMEOUT  how long to wait for the self-install to finish, seconds
#                    (default: 900 = 15 min)
#   BOOT_TIMEOUT     how long to wait for the installed system to reach
#                    multi-user.target, seconds (default: 240)
#
# The install phase uses the custom-installer live medium (kickstart-driven,
# fully unattended when the image was built with an unattended blueprint) —
# that path already talks to bootc's install code, so the bootloader choice
# comes from /usr/lib/bootc/install/40-qos.toml baked into the image.
#
# The verify phase mounts the installed disk with qemu-nbd (read-only) to
# inspect the ESP:  a systemd-boot install writes EFI/systemd/systemd-bootx64.efi
# and a loader.conf; GRUB would put EFI/fedora/grubx64.efi + shim there instead.
# We assert the systemd-boot binary exists AND grubx64.efi does NOT — the
# strongest anti-regression signal short of watching the firmware pick it.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

INSTALLER_DISK="${INSTALLER_DISK:-$REPO_ROOT/os/output/installer/qcow2/disk.qcow2}"
TARGET_SIZE="${TARGET_SIZE:-20G}"
OUT_DIR="${OUT_DIR:-$REPO_ROOT/target/perf/systemd-boot-verify}"
INSTALL_TIMEOUT="${INSTALL_TIMEOUT:-900}"
BOOT_TIMEOUT="${BOOT_TIMEOUT:-240}"

# The build/QEMU environment. Reuse os/dev/_common.sh's inenv/hmp so we run
# under the same distrobox where qemu, OVMF, socat and qemu-img live.
# shellcheck disable=SC1091
source "$REPO_ROOT/os/dev/_common.sh"

TARGET_DISK="$OUT_DIR/target.qcow2"
CONSOLE_INSTALL="$OUT_DIR/install.console.log"
CONSOLE_BOOT="$OUT_DIR/boot.console.log"
VARS_INSTALL="$OUT_DIR/vars.install.fd"
VARS_BOOT="$OUT_DIR/vars.boot.fd"
MON_INSTALL="$OUT_DIR/mon.install.sock"
MON_BOOT="$OUT_DIR/mon.boot.sock"
PID_INSTALL="$OUT_DIR/qemu.install.pid"
PID_BOOT="$OUT_DIR/qemu.boot.pid"
ESP_MNT="$OUT_DIR/mnt.esp"
LOG="$OUT_DIR/verify.log"
JSON="$OUT_DIR/result.json"

mkdir -p "$OUT_DIR"
: > "$LOG"

log() { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*" | tee -a "$LOG"; }

cleanup() {
  local pid
  for pid in "$PID_INSTALL" "$PID_BOOT"; do
    if [ -f "$pid" ]; then
      inenv "kill \$(cat '$pid') 2>/dev/null || true"
      sleep 1
      inenv "kill -9 \$(cat '$pid') 2>/dev/null || true"
      rm -f "$pid"
    fi
  done
  if mountpoint -q "$ESP_MNT" 2>/dev/null; then
    sudo umount "$ESP_MNT" 2>/dev/null || true
  fi
  sudo qemu-nbd -d /dev/nbd0 2>/dev/null || true
  rm -f "$MON_INSTALL" "$MON_BOOT"
}
trap cleanup EXIT

emit_json() {
  # $1 = phase, $2 = ok (true/false), $3 = detail
  cat > "$JSON" <<EOF
{
  "phase": "$1",
  "ok": $2,
  "detail": "$3",
  "systemd_boot_efi_present": ${SB_PRESENT:-null},
  "grub_efi_present": ${GRUB_PRESENT:-null},
  "loader_conf_present": ${LOADER_CONF_PRESENT:-null},
  "boot_ms": ${BOOT_MS:-null},
  "install_seconds": ${INSTALL_SECONDS:-null},
  "log": "$LOG",
  "target_disk": "$TARGET_DISK"
}
EOF
  cat "$JSON"
}

# --- Preconditions ----------------------------------------------------------
[ -f "$INSTALLER_DISK" ] || {
  log "installer disk not found: $INSTALLER_DISK"
  log "build it first: ./os/dev/build-custom-installer.sh"
  emit_json "precondition" false "no installer disk"; exit 3
}

# Fresh target disk every run — an "install" over an already-installed disk
# is not the invariant we test.
rm -f "$TARGET_DISK"
inenv "qemu-img create -f qcow2 '$TARGET_DISK' '$TARGET_SIZE'" >/dev/null

CODE="$(ovmf_code)"
VARS="$(ovmf_vars)"
inenv "cp -f '$VARS' '$VARS_INSTALL'"
inenv "cp -f '$VARS' '$VARS_BOOT'"

# --- Phase 1: install -------------------------------------------------------
# Boot the live installer with the blank target as /dev/vdb. The installer's
# unattended path (see os/etc/greetd/config.installer.toml + qos-install
# --unattended) runs bootc install to-disk against /dev/vdb.
log "phase 1: booting custom installer, target=$TARGET_DISK"
INSTALL_START="$(date +%s)"
rm -f "$MON_INSTALL"

# 2D display; the point is to install headlessly and read the serial log.
INSTALL_ARGS="\
  -machine q35,accel=kvm -enable-kvm -m 4096 -cpu host -smp 2 \
  -drive if=pflash,format=raw,readonly=on,file=$CODE \
  -drive if=pflash,format=raw,file=$VARS_INSTALL \
  -drive file=$INSTALLER_DISK,format=qcow2,if=virtio \
  -drive file=$TARGET_DISK,format=qcow2,if=virtio \
  -device virtio-vga -display none \
  -nic user,model=virtio-net-pci \
  -monitor unix:$MON_INSTALL,server,nowait \
  -serial file:$CONSOLE_INSTALL \
  -pidfile $PID_INSTALL -name qos-systemd-boot-verify-install"

inenv "setsid nohup qemu-system-x86_64 $INSTALL_ARGS >/dev/null 2>&1 & disown"
sleep 2
inenv "test -S '$MON_INSTALL'" || {
  log "installer monitor socket did not appear"; emit_json "install" false "qemu did not start"; exit 4
}

# Wait for one of: "install complete" reboot / bootc success line / power-off.
# The custom installer's convention is `qos-install` sends "install-complete"
# to the serial log, then triggers a poweroff (headless flow) so we can
# reboot into the target disk without any manual step.
INSTALL_DEADLINE=$(( INSTALL_START + INSTALL_TIMEOUT ))
INSTALL_STATE="unknown"
while [ "$(date +%s)" -lt "$INSTALL_DEADLINE" ]; do
  if [ -f "$CONSOLE_INSTALL" ] && sed 's/\x1b\[[0-9;]*[mGKH]//g' "$CONSOLE_INSTALL" 2>/dev/null | \
      grep -qE "install-complete|Installation complete|bootc install: success"; then
    INSTALL_STATE="complete"; break
  fi
  if grep -qE "Kernel panic|install failed|Installation failed" "$CONSOLE_INSTALL" 2>/dev/null; then
    INSTALL_STATE="failed"; break
  fi
  # qemu exited on its own = the guest powered off (install-then-poweroff).
  if ! inenv "kill -0 \$(cat '$PID_INSTALL') 2>/dev/null"; then
    INSTALL_STATE="qemu-exited"; break
  fi
  sleep 3
done
INSTALL_SECONDS=$(( $(date +%s) - INSTALL_START ))
log "phase 1: install state=$INSTALL_STATE after ${INSTALL_SECONDS}s"

# ACPI-shutdown the installer VM if it's still running.
if inenv "kill -0 \$(cat '$PID_INSTALL') 2>/dev/null"; then
  inenv "printf 'system_powerdown\n' | socat - 'unix:$MON_INSTALL'" >/dev/null 2>&1 || true
  for _ in $(seq 1 30); do
    inenv "kill -0 \$(cat '$PID_INSTALL') 2>/dev/null" || break
    sleep 1
  done
  inenv "kill -9 \$(cat '$PID_INSTALL') 2>/dev/null || true"
fi
rm -f "$PID_INSTALL"

if [ "$INSTALL_STATE" = "failed" ] || [ "$INSTALL_STATE" = "unknown" ]; then
  emit_json "install" false "$INSTALL_STATE (see $CONSOLE_INSTALL)"; exit 5
fi

# --- Phase 2: inspect the ESP ----------------------------------------------
# Mount the target disk offline and check what actually landed on the ESP.
# This is the source of truth; watching the boot menu on a serial console
# depends on how systemd-boot is configured (default `console-mode=keep` is
# quiet if there's only one entry).
log "phase 2: mounting installed target ESP for inspection"
sudo modprobe nbd max_part=8 2>/dev/null || true
sudo qemu-nbd -d /dev/nbd0 2>/dev/null || true
sudo qemu-nbd --read-only --connect=/dev/nbd0 "$TARGET_DISK"
sudo partprobe /dev/nbd0 2>/dev/null || true
sleep 1

# The ESP is FAT-32 on a bootc install; find it by TYPE.
ESP_PART=""
for p in /dev/nbd0p1 /dev/nbd0p2 /dev/nbd0p3; do
  t="$(sudo blkid -o value -s TYPE "$p" 2>/dev/null || true)"
  if [ "$t" = "vfat" ]; then ESP_PART="$p"; break; fi
done
[ -n "$ESP_PART" ] || { emit_json "inspect" false "no ESP found on installed disk"; exit 6; }

mkdir -p "$ESP_MNT"
sudo mount -o ro "$ESP_PART" "$ESP_MNT"

SB_PRESENT=false
GRUB_PRESENT=false
LOADER_CONF_PRESENT=false
if sudo test -f "$ESP_MNT/EFI/systemd/systemd-bootx64.efi" || \
   sudo test -f "$ESP_MNT/EFI/BOOT/BOOTX64.EFI"; then
  # BOOTX64.EFI can be either — check for the systemd loader.conf too.
  SB_PRESENT=true
fi
if sudo test -f "$ESP_MNT/loader/loader.conf"; then
  LOADER_CONF_PRESENT=true
fi
if sudo test -f "$ESP_MNT/EFI/fedora/grubx64.efi" || \
   sudo test -f "$ESP_MNT/EFI/fedora/grub.cfg"; then
  GRUB_PRESENT=true
fi
log "phase 2: ESP tree:"
sudo find "$ESP_MNT" -maxdepth 4 -type f 2>/dev/null | tee -a "$LOG"
log "phase 2: systemd-boot=$SB_PRESENT grub=$GRUB_PRESENT loader.conf=$LOADER_CONF_PRESENT"

sudo umount "$ESP_MNT"
sudo qemu-nbd -d /dev/nbd0 || true

if [ "$SB_PRESENT" != "true" ] || [ "$LOADER_CONF_PRESENT" != "true" ] || [ "$GRUB_PRESENT" = "true" ]; then
  emit_json "inspect" false "ESP does not match systemd-boot expectations"; exit 7
fi

# --- Phase 3: boot the installed target ------------------------------------
log "phase 3: booting installed target=$TARGET_DISK"
rm -f "$MON_BOOT"
BOOT_START="$(date +%s)"

BOOT_ARGS="\
  -machine q35,accel=kvm -enable-kvm -m 4096 -cpu host -smp 2 \
  -drive if=pflash,format=raw,readonly=on,file=$CODE \
  -drive if=pflash,format=raw,file=$VARS_BOOT \
  -drive file=$TARGET_DISK,format=qcow2,if=virtio \
  -device virtio-vga -display none \
  -nic user,model=virtio-net-pci \
  -monitor unix:$MON_BOOT,server,nowait \
  -serial file:$CONSOLE_BOOT \
  -pidfile $PID_BOOT -name qos-systemd-boot-verify-boot"

inenv "setsid nohup qemu-system-x86_64 $BOOT_ARGS >/dev/null 2>&1 & disown"
sleep 2
inenv "test -S '$MON_BOOT'" || { emit_json "boot" false "qemu did not start"; exit 8; }

BOOT_DEADLINE=$(( BOOT_START + BOOT_TIMEOUT ))
BOOT_STATE="unknown"
while [ "$(date +%s)" -lt "$BOOT_DEADLINE" ]; do
  # Two acceptable signals for a completed boot: bare `Reached target
  # multi-user.target` (system console format) or the ANSI-coloured
  # `[OK] Reached target ...` (`sed` the colour off first).
  if [ -f "$CONSOLE_BOOT" ] && sed 's/\x1b\[[0-9;]*[mGKH]//g' "$CONSOLE_BOOT" 2>/dev/null | \
      grep -qE "Reached target [^-]*(multi-user\.target|graphical\.target)"; then
    BOOT_STATE="reached"; break
  fi
  if grep -qE "Kernel panic|Failed to load kernel" "$CONSOLE_BOOT" 2>/dev/null; then
    BOOT_STATE="panic"; break
  fi
  if ! inenv "kill -0 \$(cat '$PID_BOOT') 2>/dev/null"; then
    BOOT_STATE="qemu-exited"; break
  fi
  sleep 2
done

# Extract the kernel timestamp on the multi-user reach line — that is the
# wall-clock since boot in seconds. This is the same computation
# scripts/perf/qemu-boot-measure.sh does; we reuse it here so a bootloader
# swap can be diffed against the GRUB baseline in the same units.
BOOT_MS="$(sed 's/\x1b\[[0-9;]*[mGKH]//g' "$CONSOLE_BOOT" 2>/dev/null | \
  awk '
    match($0, /\[[[:space:]]*([0-9]+\.[0-9]+)\]/, m) { last_ts=m[1] }
    /Reached target [^-]*(multi-user\.target|graphical\.target)/ {
      printf "%d", last_ts * 1000
      exit
    }
  ' || echo 0)"
[ -n "$BOOT_MS" ] || BOOT_MS=0

log "phase 3: boot state=$BOOT_STATE boot_ms=$BOOT_MS"

# ACPI-shutdown the installed VM.
if inenv "kill -0 \$(cat '$PID_BOOT') 2>/dev/null"; then
  inenv "printf 'system_powerdown\n' | socat - 'unix:$MON_BOOT'" >/dev/null 2>&1 || true
  for _ in $(seq 1 30); do
    inenv "kill -0 \$(cat '$PID_BOOT') 2>/dev/null" || break
    sleep 1
  done
  inenv "kill -9 \$(cat '$PID_BOOT') 2>/dev/null || true"
fi
rm -f "$PID_BOOT"

# --- Summary ---------------------------------------------------------------
# The verify passes if:
#   * The ESP inspection matched systemd-boot expectations, AND
#   * The installed system reached multi-user (or graphical) target.
if [ "$BOOT_STATE" = "reached" ]; then
  emit_json "verified" true "systemd-boot on ESP + boot completed in ${BOOT_MS}ms"
  exit 0
fi

emit_json "boot" false "$BOOT_STATE"
exit 9
