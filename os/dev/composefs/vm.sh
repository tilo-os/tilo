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
# Not ${QOS_SECBOOT:-1}: _common.sh has already set it to 0, and :- only fires
# when a variable is unset or empty, so that default would never apply.
QOS_SECBOOT="${QOS_CFS_SECBOOT:-1}"
QOS_TPM_DIR="$QOS_CFS_RUN/tpm"
QOS_TPM_SOCK="$QOS_TPM_DIR/swtpm-ctrl.sock"
CFS_MON="$QOS_CFS_RUN/mon.sock"
CFS_SERIAL="$QOS_CFS_RUN/serial.log"
CFS_PID="$QOS_CFS_RUN/qemu.pid"
CFS_QEMU_LOG="$QOS_CFS_RUN/qemu.log"
CFS_SSH_PORT="${CFS_SSH_PORT:-2322}"

running() { [ -f "$CFS_PID" ] && inenv "kill -0 \$(cat '$CFS_PID') 2>/dev/null"; }

# _common.sh starts swtpm without telling it to start the TPM, which leaves the
# TPM in its pre-startup state: every command returns TPM_RC_INITIALIZE (256),
# the firmware measures nothing, no event log is produced, and Linux logs
# "A TPM error (256) occurred attempting the self test" before starting the TPM
# by hand. PCRs then read as all zeroes, which looks like a firmware without TPM
# support rather than a TPM nobody started.
#
# --flags not-need-init,startup-clear makes swtpm issue TPM2_Startup(CLEAR)
# itself, so the TPM is live from the firmware's first submission onward and
# measured boot can actually happen.
cfs_swtpm_start() {
  inenv "command -v swtpm >/dev/null" || die "swtpm missing in ${QOS_DISTROBOX:-the env}"
  inenv "mkdir -p '$QOS_TPM_DIR'; \
    if ! test -S '$QOS_TPM_SOCK'; then \
      setsid nohup swtpm socket --tpmstate dir='$QOS_TPM_DIR' \
        --ctrl type=unixio,path='$QOS_TPM_SOCK' --tpm2 \
        --flags not-need-init,startup-clear \
        >'$QOS_TPM_DIR/swtpm.log' 2>&1 & disown; \
      for _ in \$(seq 1 50); do test -S '$QOS_TPM_SOCK' && break; sleep 0.1; done; \
    fi; \
    test -S '$QOS_TPM_SOCK'" \
    || die "swtpm failed to start (see $QOS_TPM_DIR/swtpm.log)"
  msg "swtpm up (TPM 2.0, started, state=$QOS_TPM_DIR)"
}
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
      # The Secure-Boot OVMF build is also the one that does measured boot: it
      # exposes the TCG2 protocol to EFI applications, which is what systemd-stub
      # needs before it will extend anything into PCR 11. The plain build leaves
      # every PCR at zero and writes no StubPcrKernelImage, so a measurement test
      # against it proves nothing. Default on here for that reason.
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
  # Fedora ships two OVMF builds and only the 4 MB one has TPM support compiled
  # in. The 2 MB build that os/dev/_common.sh finds (OVMF_CODE.fd) exposes a TCG2
  # protocol that reports zero active PCR banks, so nothing is ever measured:
  # every PCR reads as zero, no firmware event log appears, the TPM answers
  # TPM_RC_INITIALIZE, and systemd's ConditionSecurity=measured-uki is unmet. It
  # looks exactly like a UKI that failed to measure rather than a firmware that
  # cannot.
  #
  # The secboot flavour is used for its measurement support, not to enforce, and
  # is paired with the non-secboot variable store: the secboot store has
  # Microsoft's keys enrolled and refuses a UKI signed with an owner key, leaving
  # nothing bootable. A pristine store keeps Secure Boot in setup mode, which
  # still measures. Set QOS_CFS_VARS to the secboot store to test enforcement.
  local code vars fw_fmt=raw
  if inenv "test -f /usr/share/edk2/ovmf/OVMF_CODE_4M.secboot.qcow2"; then
    code=/usr/share/edk2/ovmf/OVMF_CODE_4M.secboot.qcow2
    vars="${QOS_CFS_VARS:-/usr/share/edk2/ovmf/OVMF_VARS_4M.qcow2}"
    fw_fmt=qcow2
  else
    warn "no 4M OVMF build; falling back to the 2M one, which does not measure"
    code="$(ovmf_code)"
    vars="${QOS_CFS_VARS:-$(QOS_SECBOOT=0 ovmf_vars)}"
  fi
  rm -f "$CFS_SERIAL" "$CFS_MON" "$CFS_PID"
  # The variable store is per-series too: keep it if it exists so Secure Boot
  # state (once enrolled) survives an update series.
  # The variable store is per-firmware-flavour: a secboot store and a plain one
  # are not interchangeable, and reusing the wrong one silently loses Secure Boot
  # support. Keeping it across boots of the same flavour is deliberate, so
  # enrolled keys survive an update series.
  local varstore="$QOS_CFS_RUN/vars-$fw_fmt-sb$QOS_SECBOOT.img"
  inenv "test -f '$varstore' || cp -f '$vars' '$varstore'"
  cfs_swtpm_start
  local args="$(machine_args) -enable-kvm -m $QOS_MEM -cpu host -smp $QOS_SMP \
    -drive if=pflash,format=$fw_fmt,readonly=on,file=$code \
    -drive if=pflash,format=$fw_fmt,file=$varstore \
    -drive file=$disk,format=raw,if=virtio \
    -device virtio-vga -display none \
    -nic user,model=virtio-net-pci,hostfwd=tcp:127.0.0.1:$CFS_SSH_PORT-:22 \
    -monitor unix:$CFS_MON,server,nowait -serial file:$CFS_SERIAL \
    -pidfile $CFS_PID -name qos-composefs"
  msg "boot generation $gen ($disk), TPM on, secboot-firmware=$QOS_SECBOOT, ssh on 127.0.0.1:$CFS_SSH_PORT"
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
    running || { warn "qemu exited; serial tail:"; tail -40 "$CFS_SERIAL" >&2; die "guest died"; }
    sleep 2
  done
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
