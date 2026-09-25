#!/bin/bash
# Drive owner-enrolled Secure Boot inside a VM: generate keys, sign, check that
# enrolment refuses while re-signing is off, enrol, and reboot into enforcement.
#
#   os/dev/test-secureboot.sh <desktop-qcow2> [--out DIR]
#
# The reboot happens from INSIDE the guest rather than by restarting qemu. Under
# --secboot the variable store is kept across boots (TILO_SECBOOT_KEYS=persist), so
# a restart would work too — but rebooting the guest is what a real machine does
# after enrolling, and it keeps the test independent of how the harness seeds
# pflash. Without --secboot the store is re-seeded pristine every boot, which
# would throw the enrolled keys away and make the test look like it passed for
# the wrong reason.
set -euo pipefail

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
# Each test owns a fresh variable store; never reuse a caller's enrolled store.
if [ -n "${TILO_RUN:-}" ]; then
  mkdir -m 700 -- "$TILO_RUN" || { echo "TILO_RUN must not already exist" >&2; exit 1; }
else
  TILO_RUN="$(mktemp -d /tmp/tilo-secureboot.XXXXXXXX)"
fi
export TILO_RUN
export TILO_SECBOOT_KEYS=persist
RUN="$TILO_RUN"
NONCE="$(python3 -c 'import uuid; print(uuid.uuid4())')"
OUT="${OUT:-$RUN/secureboot-results}"
mkdir -p "$OUT"

say() { printf '\n### %s\n' "$*"; }
SSHOPTS=(-F /dev/null -p "${TILO_SSH_PORT:-2222}" -o StrictHostKeyChecking=no
         -o UserKnownHostsFile=/dev/null -o ConnectTimeout=8 -o LogLevel=ERROR
         -o BatchMode=no -o NumberOfPasswordPrompts=1
         -o PreferredAuthentications=password -o PubkeyAuthentication=no
         -o KbdInteractiveAuthentication=no)
# Automation must never fall through to the desktop's graphical askpass
# helper; sshpass owns the one permitted password prompt.
unset SSH_ASKPASS SSH_ASKPASS_REQUIRE
GUEST="${TILO_VM_USER:-tester}@127.0.0.1"
GUEST_PW="${TILO_VM_PASS:-tilo}"
gssh() { sshpass -p "$GUEST_PW" ssh "${SSHOPTS[@]}" "$GUEST" "$@"; }
runscript() {
  printf '%s\n' "$2" > "$OUT/$1.sh"
  sshpass -p "$GUEST_PW" ssh "${SSHOPTS[@]}" "$GUEST" 'cat > /tmp/step.sh' < "$OUT/$1.sh" || return 1
  sshpass -p "$GUEST_PW" ssh "${SSHOPTS[@]}" "$GUEST" \
    "echo $GUEST_PW | sudo -S -p '' bash -euo pipefail /tmp/step.sh 2>&1"
}
wait_ssh() {
  local i
  for i in $(seq 1 60); do
    gssh true >/dev/null 2>&1 && { echo "ssh up after $((i*10))s"; return 0; }
    sleep 10
  done
  return 1
}

cleanup() { TILO_DISK="$DESKTOP" "$VM" stop >/dev/null 2>&1 || true; }
trap cleanup EXIT

say "boot with the Secure-Boot firmware (setup mode, SMM enforced)"
TILO_DISK="$DESKTOP" TILO_SECBOOT=1 "$VM" boot --2d --secboot || exit 1
wait_ssh || { echo "ssh never came up"; "$VM" serial | tail -60; exit 1; }

say "firmware state before anything"
runscript before '
/usr/libexec/tilo-secureboot status
ls /sys/firmware/efi/efivars/ | grep -iE "SecureBoot|SetupMode|^PK|^KEK|^db" || echo none
' | tee "$OUT/01-before.txt"

# A setup-mode owner-only test deliberately has no foreign-key capture claim.
evidence() {
  gssh "python3 - collect '$NONCE' '$1'" < "$DEV_DIR/secureboot-evidence.py" > "$OUT/$1.json"
}
evidence before
python3 "$DEV_DIR/secureboot-evidence.py" before "$NONCE" "$OUT/before.json"

say "generate keys, sign, verify"
runscript keys '
set -e
/usr/libexec/tilo-secureboot generate-keys "tilo-vm"
/usr/libexec/tilo-secureboot sign
/usr/libexec/tilo-secureboot verify
/usr/libexec/tilo-secureboot status
' | tee "$OUT/03-sign.txt"

say "enrol must REFUSE while the re-signing units are off"
runscript refuse '
systemctl disable tilo-secureboot-sign.path tilo-secureboot-sign.service
if /usr/libexec/tilo-secureboot enroll --yes --no-vendor-keys > /tmp/tilo-enroll-refusal.txt 2>&1; then
  cat /tmp/tilo-enroll-refusal.txt
  echo "ERROR: enrollment succeeded with re-signing disabled" >&2
  exit 1
else
  rc=$?
  cat /tmp/tilo-enroll-refusal.txt
  test "$rc" -eq 1
  grep -Fq "the re-signing units are not both enabled" /tmp/tilo-enroll-refusal.txt
fi
' | tee "$OUT/04-refusal.txt"

evidence refused
python3 "$DEV_DIR/secureboot-evidence.py" refused "$NONCE" "$OUT/before.json" "$OUT/refused.json"

say "arm re-signing, then enrol"
runscript enroll '
set -x
systemctl enable tilo-secureboot-sign.path tilo-secureboot-sign.service tilo-secureboot-sign-shutdown.service
/usr/libexec/tilo-secureboot enroll --yes --no-vendor-keys
/usr/libexec/tilo-secureboot status
' | tee "$OUT/05-enroll.txt"

say "reboot from inside the guest so the pflash store survives"
# Queue reboot so SSH reports successful scheduling before the connection drops.
runscript reboot 'systemd-run --on-active=2s /usr/bin/systemctl reboot' | tee "$OUT/06-reboot.txt"
sleep 25
wait_ssh || {
  echo "GUEST DID NOT COME BACK — this is the brick case, and the escape hatch is"
  echo "clearing the Secure Boot keys in firmware setup."
  "$VM" serial | tail -80 > "$OUT/06-serial.txt"; tail -50 "$OUT/06-serial.txt"; exit 1; }

say "after the reboot: is Secure Boot enforcing, on our signed kernel?"
runscript after '
/usr/libexec/tilo-secureboot status
cat /sys/kernel/security/lockdown 2>&1 || true
/usr/libexec/tilo-security-audit | grep -iE "secure boot|lockdown|owner"
' | tee "$OUT/07-after.txt"

evidence after
python3 "$DEV_DIR/secureboot-evidence.py" after "$NONCE" "$OUT/before.json" "$OUT/after.json"

"$VM" serial > "$OUT/08-serial.txt" 2>&1 || true
TILO_DISK="$DESKTOP" "$VM" stop

say "verdict"
echo "PASS: fresh guest reboot, SecureBoot=1 SetupMode=0; owner-only VM test."
echo "No foreign-key preservation, TPM, or installation-readiness claim."
echo "artifacts: $OUT"
