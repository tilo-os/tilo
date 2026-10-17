#!/bin/bash
# Prove the sealed command line is measured — and prove the proof can fail.
#
#   ./os/dev/test-measured-boot.sh [DISK]
#
# Boots the qcow2 with a software TPM, runs the image's own
# /usr/libexec/tilo-rollback-boot-check, then blanks `tilo.generation=` out of the
# installed UKI in place, reboots, and runs it again.
#
# The second half is the point. Every symptom of a UKI that never measured —
# PCR 11 at its reset value, no StubPcrKernelImage, ConditionSecurity=measured-uki
# unmet — is also a symptom of a firmware that cannot measure, or a software TPM
# nobody started. A check that only ever passes cannot tell the difference, so this
# requires the negative run to fail on the *generation* assertions while PCR 11
# still holds a non-zero measurement. Blanking the token in place at the same byte
# length is what makes that separable: the PE layout is untouched, the boot measures
# exactly as before, and the one thing that changed is the value measured.
#
# Exit 0 only if the positive run passed AND the negative run failed for the right
# reason.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VM="$HERE/vm.sh"
DISK="${1:-}"

# shellcheck source=/dev/null
source "$HERE/_common.sh"
set +e   # this script reports rather than aborts; _common.sh sets -e

[ -n "$DISK" ] || DISK="$TILO_DISK"
[ -f "$DISK" ] || die "no disk image at $DISK (build one with os/dev/build-image.sh)"

OUT="${OUT:-$TILO_RUN/measured-boot}"
mkdir -p "$OUT"

SSHOPTS=(-F /dev/null -p "$TILO_SSH_PORT" -o StrictHostKeyChecking=no
         -o UserKnownHostsFile=/dev/null -o ConnectTimeout=8 -o LogLevel=ERROR
         -o BatchMode=no -o NumberOfPasswordPrompts=1
         -o PreferredAuthentications=password -o PubkeyAuthentication=no
         -o KbdInteractiveAuthentication=no)
# Automation must never fall through to the desktop's graphical askpass
# helper; sshpass owns the one permitted password prompt.
unset SSH_ASKPASS SSH_ASKPASS_REQUIRE
GUEST="$TILO_USER@127.0.0.1"
g() { sshpass -p "$TILO_PASS" ssh "${SSHOPTS[@]}" "$GUEST" "$@"; }
groot() { g "echo $TILO_PASS | sudo -S -p '' $*" ; }

say() { printf '\n\033[1;35m### %s\033[0m\n' "$*"; }

wait_ssh() {
  local i
  for i in $(seq 1 90); do
    g true >/dev/null 2>&1 && { msg "ssh up after $((i*5))s"; return 0; }
    sleep 5
  done
  return 1
}
wait_ssh_gone() {
  local i
  for i in $(seq 1 60); do
    g true >/dev/null 2>&1 || return 0
    sleep 2
  done
  return 1
}

cleanup() { TILO_DISK="$DISK" "$VM" stop >/dev/null 2>&1; }
trap cleanup EXIT

# --- positive run -------------------------------------------------------------
say "boot with a software TPM (4 MB OVMF — the only build that measures)"
TILO_DISK="$DISK" TILO_TPM=1 "$VM" boot --2d --tpm || die "the VM did not start"
wait_ssh || { warn "ssh never came up"; "$VM" serial | tail -60; exit 1; }

say "the command line the UKI sealed, read back out of the PE on the ESP"
groot "/usr/libexec/tilo-uki-cmdline show" | tee "$OUT/sealed-cmdline.txt"

say "what the kernel was actually given"
g 'cat /proc/cmdline' | tee "$OUT/proc-cmdline.txt"

say "boot-side checks, generation present"
groot /usr/libexec/tilo-rollback-boot-check > "$OUT/check-positive.txt" 2>&1
positive_rc=$?
cat "$OUT/check-positive.txt"

# --- negative run -------------------------------------------------------------
say "blank tilo.generation= out of the UKI, in place, at the same length"
groot "/usr/libexec/tilo-uki-cmdline blank tilo.generation" | tee "$OUT/blank.txt"
grep -q '^blanked tilo.generation' "$OUT/blank.txt" \
  || { warn "the generation was not blanked; the negative run would prove nothing"; exit 1; }

say "reboot onto the UKI with no generation"
groot "systemctl reboot" >/dev/null 2>&1
wait_ssh_gone || warn "the guest did not go down; the reboot may not have taken"
wait_ssh || { warn "the guest did not come back — a UKI edited at the wrong length
  changes the PE layout and does not load. Serial:"; "$VM" serial | tail -40; exit 1; }

say "boot-side checks, generation absent"
groot /usr/libexec/tilo-rollback-boot-check > "$OUT/check-negative.txt" 2>&1
negative_rc=$?
cat "$OUT/check-negative.txt"

# --- verdict ------------------------------------------------------------------
say "verdict"
fails=0
verdict() { if [ "$1" = 0 ]; then printf '[ OK ]   %s\n' "$2"; else printf '[FAIL]   %s\n' "$2"; fails=$((fails+1)); fi; }

grep -q 'tilo\.generation=[0-9]' "$OUT/sealed-cmdline.txt"; verdict $? "the UKI seals tilo.generation="
[ "$positive_rc" = 0 ]; verdict $? "the boot-side check passes with the generation sealed"
[ "$negative_rc" != 0 ]; verdict $? "the boot-side check fails with the generation blanked"

# The discrimination this whole script exists for: it must fail because the
# generation is gone, not because nothing was measured.
grep -qE '^\[FAIL\].*no tilo\.generation= on the kernel command line' "$OUT/check-negative.txt"
verdict $? "  ...and it fails on the missing generation specifically"
grep -qE '^\[ OK \].*PCR 11 holds a non-zero measurement' "$OUT/check-negative.txt"
verdict $? "  ...while PCR 11 still holds a measurement (so this is not 'nothing measured')"
grep -qE '^\[ OK \].*systemd agrees this is a measured UKI' "$OUT/check-negative.txt"
verdict $? "  ...and systemd still calls it a measured UKI"

printf '\nevidence in %s\n' "$OUT"
if [ "$fails" = 0 ]; then
  msg "measured boot is real on this image, and the check can fail"
  exit 0
fi
warn "$fails verdict(s) failed"
exit 1
