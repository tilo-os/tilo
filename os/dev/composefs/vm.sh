#!/bin/bash
# Boot a raw composefs disk image headlessly, with a software TPM attached.
#
#   ./os/dev/composefs/vm.sh boot [GENERATION] [--serial-socket]
#                                                start (detached), swtpm on
#   ./os/dev/composefs/vm.sh console             path of the serial socket
#   ./os/dev/composefs/vm.sh wait                block until sshd answers
#   ./os/dev/composefs/vm.sh ssh -- CMD...       run a command in the guest
#   ./os/dev/composefs/vm.sh serial [GREP]       dump the serial console log
#   ./os/dev/composefs/vm.sh reboot              reboot the guest and wait
#   ./os/dev/composefs/vm.sh stop                power off
#
# os/dev/vm.sh boots the qcow2 os/dev/disk-image.sh produces; this boots the
# raw file `bootc install to-disk --via-loopback` writes, and always attaches the
# TPM because a measured command line is the whole point of the exercise. The
# TPM state directory is per-generation-series, not per-boot, so PCR values can
# be compared across an update series.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$HERE/common.sh"

TILO_TPM=1
# Pristine (setup mode) but kept across boots: the store is per-generation-series,
# not per-boot, so anything enrolled during a series is still there afterwards.
# Assigned from TILO_CFS_SECBOOT_KEYS, not from itself: _common.sh has already set
# TILO_SECBOOT_KEYS, and `:-` only fires on unset or empty, so a default written
# that way would never apply.
TILO_SECBOOT_KEYS="${TILO_CFS_SECBOOT_KEYS:-persist}"
TILO_TPM_DIR="$TILO_CFS_RUN/tpm"
TILO_TPM_SOCK="$TILO_TPM_DIR/swtpm-ctrl.sock"
CFS_MON="$TILO_CFS_RUN/mon.sock"
CFS_SERIAL="$TILO_CFS_RUN/serial.log"
CFS_CONSOLE="$TILO_CFS_RUN/console.sock"
CFS_PID="$TILO_CFS_RUN/qemu.pid"
CFS_QEMU_LOG="$TILO_CFS_RUN/qemu.log"
CFS_SSH_PORT="${CFS_SSH_PORT:-2322}"

running() { [ -f "$CFS_PID" ] && inenv "kill -0 \$(cat '$CFS_PID') 2>/dev/null"; }

chmp() { inenv "printf '%s\n' \"$1\" | socat - 'unix:$CFS_MON'"; }

gsh() {
  env -u SSH_ASKPASS -u SSH_ASKPASS_REQUIRE \
    sshpass -p tilo ssh -F /dev/null -p "$CFS_SSH_PORT" \
    -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o ConnectTimeout=5 -o LogLevel=ERROR -o BatchMode=no \
    -o NumberOfPasswordPrompts=1 -o PreferredAuthentications=password \
    -o PubkeyAuthentication=no -o KbdInteractiveAuthentication=no \
    root@127.0.0.1 "$@"
}

cmd_boot() {
  local gen="" disk serial_mode=file
  while [ $# -gt 0 ]; do
    case "$1" in
      # A serial line something can type into. `-serial file:` can only be
      # read, and repair mode is an interactive console on /dev/console — so
      # without this there is no way to show it doing anything, only that it
      # started. os/dev/serial-console.py drives the socket.
      #
      # SOMETHING MUST STAY ATTACHED. qemu writes the guest's console into the
      # socket, and with no reader it blocks once the pipe fills — which stops
      # the guest mid-boot, looking exactly like a hang in whatever it was
      # doing. Use this only while a driver is connected, and boot without it
      # for anything that only needs to read (`vm.sh serial`).
      --serial-socket) serial_mode=socket ;;
      # Enforcement is a separate axis from measurement: the measuring firmware
      # is selected unconditionally (os/dev/_common.sh), and this only asks for a
      # variable store that survives a restart so keys enrolled in the guest
      # stay enrolled across an update series.
      --secboot) TILO_SECBOOT=1 ;;
      --no-secboot) TILO_SECBOOT=0 ;;
      [0-9]*) gen="$1" ;;
      *) die "boot: unknown arg $1" ;;
    esac
    shift
  done
  gen="${gen:-1}"
  disk="$TILO_CFS_RUN/gen$gen.raw"
  [ -f "$disk" ] || die "no disk for generation $gen ($disk) — run install.sh $gen"
  running && die "a spike VM is already running (pid $(cat "$CFS_PID")); stop it first"
  mkdir -p "$TILO_CFS_RUN"
  # TILO_CFS_VARS overrides the store outright, for testing a prepared one.
  [ -n "${TILO_CFS_VARS:-}" ] && TILO_SECBOOT_KEYS="$TILO_CFS_VARS"
  firmware_setup
  rm -f "$CFS_SERIAL" "$CFS_MON" "$CFS_PID"
  local varstore; varstore="$(firmware_varstore "$TILO_CFS_RUN")"
  swtpm_start
  local serial_arg
  if [ "$serial_mode" = socket ]; then
    rm -f "$CFS_CONSOLE"
    serial_arg="-serial unix:$CFS_CONSOLE,server=on,wait=off"
  else
    serial_arg="-serial file:$CFS_SERIAL"
  fi
  local args="$(machine_args) -enable-kvm -m $TILO_MEM -cpu host -smp $TILO_SMP \
    $(pflash_args "$varstore") \
    -drive file=$disk,format=raw,if=virtio \
    -device virtio-vga -display none \
    -nic user,model=virtio-net-pci,hostfwd=tcp:127.0.0.1:$CFS_SSH_PORT-:22 \
    -monitor unix:$CFS_MON,server,nowait $serial_arg \
    -pidfile $CFS_PID -name tilo-composefs"
  msg "boot generation $gen ($disk), TPM on, varstore keys=$TILO_SECBOOT_KEYS, ssh on 127.0.0.1:$CFS_SSH_PORT, serial=$serial_mode"
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
  console) printf '%s\n' "$CFS_CONSOLE";;
  wait) cmd_wait;;
  ssh) gsh "$@";;
  reboot) cmd_reboot;;
  serial) cmd_serial "$@";;
  status) if running; then echo "running (pid $(cat "$CFS_PID"))"; else echo stopped; fi;;
  stop) cmd_stop;;
  *) sed -n '2,16p' "$0";;
esac
