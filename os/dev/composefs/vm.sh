#!/bin/bash
# Boot a raw composefs disk image headlessly, with a software TPM attached.
#
#   ./os/dev/composefs/vm.sh boot [GENERATION]   start (detached), swtpm on
#   ./os/dev/composefs/vm.sh wait                block until sshd answers
#   ./os/dev/composefs/vm.sh ssh -- CMD...       run a command in the guest
#   ./os/dev/composefs/vm.sh serial [GREP]       dump the serial console log
#   ./os/dev/composefs/vm.sh reboot              reboot the guest and wait
#   ./os/dev/composefs/vm.sh stop                power off
#
# os/dev/vm.sh boots the qcow2 that bootc-image-builder produces; this boots the
# raw file `bootc install to-disk --via-loopback` writes, and always attaches the
# TPM because a measured command line is the whole point of the exercise. The
# TPM state directory is per-generation-series, not per-boot, so PCR values can
# be compared across an update series.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$HERE/common.sh"

QOS_TPM=1
# Pristine (setup mode) but kept across boots: the store is per-generation-series,
# not per-boot, so anything enrolled during a series is still there afterwards.
QOS_SECBOOT_KEYS="${QOS_SECBOOT_KEYS:-persist}"
QOS_TPM_DIR="$QOS_CFS_RUN/tpm"
QOS_TPM_SOCK="$QOS_TPM_DIR/swtpm-ctrl.sock"
CFS_MON="$QOS_CFS_RUN/mon.sock"
CFS_SERIAL="$QOS_CFS_RUN/serial.log"
CFS_PID="$QOS_CFS_RUN/qemu.pid"
CFS_QEMU_LOG="$QOS_CFS_RUN/qemu.log"
CFS_SSH_PORT="${CFS_SSH_PORT:-2322}"

running() { [ -f "$CFS_PID" ] && inenv "kill -0 \$(cat '$CFS_PID') 2>/dev/null"; }

chmp() { inenv "printf '%s\n' \"$1\" | socat - 'unix:$CFS_MON'"; }

gsh() {
  sshpass -p quickdesktop ssh -p "$CFS_SSH_PORT" \
    -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o ConnectTimeout=5 -o LogLevel=ERROR root@127.0.0.1 "$@"
}

cmd_boot() {
  local gen="" disk
  while [ $# -gt 0 ]; do
    case "$1" in
      # Enforcement is a separate axis from measurement: the measuring firmware
      # is selected unconditionally (os/dev/_common.sh), and this only asks for a
      # variable store that survives a restart so keys enrolled in the guest
      # stay enrolled across an update series.
      --secboot) QOS_SECBOOT=1 ;;
      --no-secboot) QOS_SECBOOT=0 ;;
      [0-9]*) gen="$1" ;;
      *) die "boot: unknown arg $1" ;;
    esac
    shift
  done
  gen="${gen:-1}"
  disk="$QOS_CFS_RUN/gen$gen.raw"
  [ -f "$disk" ] || die "no disk for generation $gen ($disk) — run install.sh $gen"
  running && die "a spike VM is already running (pid $(cat "$CFS_PID")); stop it first"
  mkdir -p "$QOS_CFS_RUN"
  # QOS_CFS_VARS overrides the store outright, for testing a prepared one.
  [ -n "${QOS_CFS_VARS:-}" ] && QOS_SECBOOT_KEYS="$QOS_CFS_VARS"
  firmware_setup
  rm -f "$CFS_SERIAL" "$CFS_MON" "$CFS_PID"
  local varstore; varstore="$(firmware_varstore "$QOS_CFS_RUN")"
  swtpm_start
  local args="$(machine_args) -enable-kvm -m $QOS_MEM -cpu host -smp $QOS_SMP \
    $(pflash_args "$varstore") \
    -drive file=$disk,format=raw,if=virtio \
    -device virtio-vga -display none \
    -nic user,model=virtio-net-pci,hostfwd=tcp:127.0.0.1:$CFS_SSH_PORT-:22 \
    -monitor unix:$CFS_MON,server,nowait -serial file:$CFS_SERIAL \
    -pidfile $CFS_PID -name qos-composefs"
  msg "boot generation $gen ($disk), TPM on, varstore keys=$QOS_SECBOOT_KEYS, ssh on 127.0.0.1:$CFS_SSH_PORT"
  inenv "setsid nohup qemu-system-x86_64 $args $(tpm_qemu_args) $(secboot_qemu_args) >'$CFS_QEMU_LOG' 2>&1 & disown"
  sleep 1
  inenv "test -S '$CFS_MON'" || { cat "$CFS_QEMU_LOG" >&2; die "boot failed"; }
  msg "up. serial=$CFS_SERIAL monitor=$CFS_MON"
}

cmd_wait() {
  local i
  msg "waiting for sshd ..."
  for i in $(seq 1 120); do
    if gsh true 2>/dev/null; then msg "ssh up (~$((i*2))s)"; return 0; fi
    running || { boot_diagnose "$CFS_SERIAL" || true
      warn "qemu exited; serial tail:"; tail -40 "$CFS_SERIAL" >&2; die "guest died"; }
    sleep 2
  done
  boot_diagnose "$CFS_SERIAL" || true
  warn "serial tail:"; tr -d '\000' < "$CFS_SERIAL" | tail -60 >&2
  die "timed out waiting for ssh"
}

cmd_reboot() {
  msg "rebooting the guest"
  gsh "systemctl reboot" 2>/dev/null || true
  sleep 8
  cmd_wait
}

cmd_serial() {
  [ -f "$CFS_SERIAL" ] || die "no serial log yet"
  if [ -n "${1:-}" ]; then tr -d '\000' < "$CFS_SERIAL" | sed 's/\r//g' | grep -aiE "$1"
  else tr -d '\000' < "$CFS_SERIAL" | sed 's/\r//g'; fi
}

cmd_stop() {
  if running; then chmp quit >/dev/null 2>&1 || true; sleep 1; fi
  if running; then inenv "kill \$(cat '$CFS_PID') 2>/dev/null" || true; fi
  rm -f "$CFS_PID"
  swtpm_stop
  msg "stopped"
}

sub="${1:-}"; shift || true
case "$sub" in
  boot) cmd_boot "$@";;
  wait) cmd_wait;;
  ssh) gsh "$@";;
  reboot) cmd_reboot;;
  serial) cmd_serial "$@";;
  status) if running; then echo "running (pid $(cat "$CFS_PID"))"; else echo stopped; fi;;
  stop) cmd_stop;;
  *) sed -n '2,16p' "$0";;
esac
