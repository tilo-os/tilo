#!/bin/bash
# Drive owner-enrolled Secure Boot inside a VM: generate keys, sign, check that
# enrolment refuses while re-signing is off, enrol, and reboot into enforcement.
#
#   os/dev/test-secureboot.sh <desktop-qcow2> [--out DIR]
#
# The reboot happens from INSIDE the guest, not by restarting qemu: vm.sh reseeds
# the pflash variable store from the firmware template on every boot, so a
# restart would throw the enrolled keys away and the test would look like it
# passed for the wrong reason.
set -uo pipefail

DEV_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VM="$DEV_DIR/vm.sh"
DESKTOP="${1:?usage: test-secureboot.sh <desktop-qcow2> [--out DIR]}"
shift
OUT=""
while [ $# -gt 0 ]; do
  case "$1" in
    --out) OUT="$2"; shift 2 ;;
    *) echo "unknown arg $1" >&2; exit 2 ;;
  esac
done
RUN="${QOS_RUN:-/tmp/qos-vm}"
OUT="${OUT:-$RUN/secureboot-results}"
mkdir -p "$OUT"

say() { printf '\n### %s\n' "$*"; }
SSHOPTS=(-p "${QOS_SSH_PORT:-2222}" -o StrictHostKeyChecking=no
         -o UserKnownHostsFile=/dev/null -o ConnectTimeout=8 -o LogLevel=ERROR)
GUEST="${QOS_VM_USER:-quickuser}@127.0.0.1"
GUEST_PW="${QOS_VM_PASS:-quickdesktop}"
gssh() { sshpass -p "$GUEST_PW" ssh "${SSHOPTS[@]}" "$GUEST" "$@"; }
runscript() {
  printf '%s\n' "$2" > "$OUT/$1.sh"
  sshpass -p "$GUEST_PW" ssh "${SSHOPTS[@]}" "$GUEST" 'cat > /tmp/step.sh' < "$OUT/$1.sh" || return 1
  sshpass -p "$GUEST_PW" ssh "${SSHOPTS[@]}" "$GUEST" \
    "echo $GUEST_PW | sudo -S -p '' bash /tmp/step.sh 2>&1"
}
wait_ssh() {
  local i
  for i in $(seq 1 60); do
    gssh true >/dev/null 2>&1 && { echo "ssh up after $((i*10))s"; return 0; }
    sleep 10
  done
  return 1
}

cleanup() { QOS_DISK="$DESKTOP" "$VM" stop >/dev/null 2>&1 || true; }
trap cleanup EXIT

say "boot with the Secure-Boot firmware (setup mode, SMM enforced)"
QOS_DISK="$DESKTOP" QOS_SECBOOT=1 "$VM" boot --2d --secboot || exit 1
wait_ssh || { echo "ssh never came up"; "$VM" serial | tail -60; exit 1; }

say "firmware state before anything"
runscript before '
/usr/libexec/qos-secureboot status
mokutil --sb-state 2>&1 || true
ls /sys/firmware/efi/efivars/ | grep -iE "SecureBoot|SetupMode|^PK|^KEK|^db" || echo none
' | tee "$OUT/01-before.txt"

say "capture vendor keys (OVMF setup mode has none — expect it to say so)"
runscript capture '/usr/libexec/qos-secureboot capture-vendor-keys' | tee "$OUT/02-capture.txt"

say "generate keys, sign, verify"
runscript keys '
set -e
/usr/libexec/qos-secureboot generate-keys "qos-vm"
/usr/libexec/qos-secureboot sign
/usr/libexec/qos-secureboot verify
/usr/libexec/qos-secureboot status
' | tee "$OUT/03-sign.txt"

say "enrol must REFUSE while the re-signing units are off"
runscript refuse '
systemctl disable qos-secureboot-sign.path qos-secureboot-sign.service >/dev/null 2>&1 || true
/usr/libexec/qos-secureboot enroll --yes --no-vendor-keys; echo "exit=$?"
' | tee "$OUT/04-refusal.txt"

say "arm re-signing, then enrol"
runscript enroll '
set -x
systemctl enable qos-secureboot-sign.path qos-secureboot-sign.service
/usr/libexec/qos-secureboot enroll --yes --no-vendor-keys
/usr/libexec/qos-secureboot status
' | tee "$OUT/05-enroll.txt"

say "reboot from inside the guest so the pflash store survives"
runscript reboot 'systemctl reboot' >/dev/null 2>&1 || true
sleep 25
wait_ssh || {
  echo "GUEST DID NOT COME BACK — this is the brick case, and the escape hatch is"
  echo "clearing the Secure Boot keys in firmware setup."
  "$VM" serial | tail -80 > "$OUT/06-serial.txt"; tail -50 "$OUT/06-serial.txt"; exit 1; }

say "after the reboot: is Secure Boot enforcing, on our signed kernel?"
runscript after '
/usr/libexec/qos-secureboot status
mokutil --sb-state 2>&1 || true
cat /sys/kernel/security/lockdown 2>&1 || true
/usr/libexec/qos-security-audit | grep -iE "secure boot|lockdown|owner"
' | tee "$OUT/07-after.txt"

"$VM" serial > "$OUT/08-serial.txt" 2>&1 || true
QOS_DISK="$DESKTOP" "$VM" stop

say "verdict"
grep -iE "SecureBoot=|SecureBoot (enabled|disabled)|lockdown|armed" "$OUT/07-after.txt" \
  || echo "(nothing matched)"
echo "artifacts: $OUT"
