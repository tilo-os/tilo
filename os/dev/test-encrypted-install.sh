#!/bin/bash
# Drive an encrypted install inside a booted Tilo VM, then boot the result.
#
#   os/dev/test-encrypted-install.sh <label> <desktop-qcow2> [options]
#
#     --no-tpm              run without a software TPM
#     --passphrase P        enrol P as an unlock secret, and type it at the
#                           boot prompt — the end-to-end proof that early
#                           userspace is present and can open the root
#     --no-initrd           delete the initrd line from the installed boot
#                           entry before booting it, reproducing the state the
#                           image shipped in before it had an initramfs
#     --out DIR             where to put logs and screenshots
#
# The installer host is an ordinary desktop image booted with a blank second
# disk (`vm.sh boot --attach`), so this needs no installer medium. It drives the
# guest's own /usr/libexec/tilo-disk-encrypt, so what it proves is the shipped
# path and not a re-implementation of it.
#
# ## What the verdict is taken from
#
# Whether the installed system reached a login, established by logging into it —
# not whether a prompt was spotted in a log. Those are different claims, and
# confusing them is how a run once reported "NO passphrase prompt within 150s"
# about a system that was sitting at its passphrase prompt the whole time: the
# installed kernel had no console= karg, so the serial line carried the GRUB
# menu and then nothing at all, and the harness read that silence as absence.
#
# So: the installed boot entry is instrumented with a serial console below (see
# `layout`), every step's exit status is checked, and before drawing any
# conclusion about the prompt the run first establishes whether it can see the
# installed console at all. "I did not see it" is reported as that, and never as
# "it did not happen".
set -uo pipefail

DEV_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VM="$DEV_DIR/vm.sh"

LABEL="${1:?usage: test-encrypted-install.sh <label> <desktop-qcow2> [options]}"
DESKTOP="${2:?missing desktop qcow2}"
shift 2
TPM_FLAG=--tpm
PASSPHRASE=""
STRIP_INITRD=0
OUT=""
while [ $# -gt 0 ]; do
  case "$1" in
    --no-tpm) TPM_FLAG=--no-tpm; shift ;;
    --passphrase) PASSPHRASE="$2"; shift 2 ;;
    --no-initrd) STRIP_INITRD=1; shift ;;
    --out) OUT="$2"; shift 2 ;;
    *) echo "unknown arg $1" >&2; exit 2 ;;
  esac
done

RUN="${TILO_RUN:-/tmp/tilo-vm}"
TARGET="$RUN/$LABEL-target.qcow2"
OUT="${OUT:-$RUN/$LABEL-results}"
mkdir -p "$OUT" "$RUN"
rm -f "$TARGET"
# One TPM state directory per scenario, or a later run unseals against an
# earlier run's TPM and proves nothing about its own enrolment.
export TILO_TPM_DIR="$RUN/tpm-$LABEL"
rm -rf "$TILO_TPM_DIR"

say() { printf '\n### %s\n' "$*"; }
die() { printf '\n!!! %s\n' "$*" >&2; exit 1; }

SSHOPTS=(-F /dev/null -p "${TILO_SSH_PORT:-2222}" -o StrictHostKeyChecking=no
         -o UserKnownHostsFile=/dev/null -o ConnectTimeout=8 -o LogLevel=ERROR
         -o BatchMode=no -o NumberOfPasswordPrompts=1
         -o PreferredAuthentications=password -o PubkeyAuthentication=no
         -o KbdInteractiveAuthentication=no)
GUEST="${TILO_VM_USER:-quickuser}@127.0.0.1"
GUEST_PW="${TILO_VM_PASS:-tilo}"
gssh() {
  env -u SSH_ASKPASS -u SSH_ASKPASS_REQUIRE \
    sshpass -p "$GUEST_PW" ssh "${SSHOPTS[@]}" "$GUEST" "$@"
}
# Run a script as root in the guest. The password reaches sudo on stdin and the
# script comes from a file, so the two never contend for stdin.
runscript() {
  printf '%s\n' "$2" > "$OUT/$1.sh"
  env -u SSH_ASKPASS -u SSH_ASKPASS_REQUIRE sshpass -p "$GUEST_PW" ssh "${SSHOPTS[@]}" "$GUEST" 'cat > /tmp/step.sh' < "$OUT/$1.sh" || return 1
  env -u SSH_ASKPASS -u SSH_ASKPASS_REQUIRE sshpass -p "$GUEST_PW" ssh "${SSHOPTS[@]}" "$GUEST" \
    "echo $GUEST_PW | sudo -S -p '' bash /tmp/step.sh 2>&1"
}
# One install step: the whole output to a file, a tail of it to the terminal,
# and the step's own exit status preserved for the caller. Piping straight into
# `tee | tail` — as this used to — throws the status away, so a step that died
# partway through looked exactly like one that succeeded.
step() {
  local name="$1" out="$2" script="$3" tailn="${4:-20}" rc
  runscript "$name" "$script" > "$out" 2>&1
  rc=$?
  tail -n "$tailn" "$out"
  return $rc
}
wait_ssh() {
  local tries="${1:-60}" i
  for i in $(seq 1 "$tries"); do
    gssh true >/dev/null 2>&1 && { echo "ssh up after $((i*10))s"; return 0; }
    sleep 10
  done
  return 1
}
serial_has() { "$VM" serial 2>/dev/null | grep -aqiE "$1"; }

cleanup() { TILO_DISK="$DESKTOP" "$VM" stop >/dev/null 2>&1 || true; }
trap cleanup EXIT

say "boot the installer host ($DESKTOP, $TPM_FLAG)"
TILO_DISK="$DESKTOP" "$VM" boot --2d "$TPM_FLAG" --attach "$TARGET" || exit 1
wait_ssh || { echo "ssh never came up"; "$VM" serial | tail -60; exit 1; }

say "guest facts"
step facts "$OUT/01-facts.txt" '
ls -la /usr/lib/modules/*/initramfs.img 2>&1 || echo "NO INITRAMFS in /usr/lib/modules"
ls /usr/lib/modules
ls /dev/tpm* 2>&1 || echo "no tpm device"
cat /etc/kernel/install.conf 2>&1 || echo "no /etc/kernel/install.conf (expected)"
bash /usr/libexec/tilo/boot-chain-test.sh 2>&1 || true
' 20 || echo "(facts step reported a non-zero status; continuing)"

say "copy the booted image into guest container storage"
step copy "$OUT/01b-copy.txt" 'bootc image copy-to-storage' 3 \
  || die "could not copy the image into guest container storage"

say "prepare the encrypted volume"
step prepare "$OUT/02-prepare.txt" \
  '/usr/libexec/tilo-disk-encrypt prepare /dev/vdb --filesystem btrfs' 20 \
  || die "prepare failed — see $OUT/02-prepare.txt"
MNT=$(sed -n 's/^MNT=//p' "$OUT/02-prepare.txt" | tr -d '\r')
LUKS=$(sed -n 's/^LUKS_PART=//p' "$OUT/02-prepare.txt" | tr -d '\r')
KEY=$(sed -n 's/^KEY_FILE=//p' "$OUT/02-prepare.txt" | tr -d '\r')
RUUID=$(sed -n 's/^ROOT_FS_UUID=//p' "$OUT/02-prepare.txt" | tr -d '\r')
KARGS=$(sed -n 's/^KARGS=//p' "$OUT/02-prepare.txt" | tr -d '\r')
[ -n "$MNT" ] && [ -n "$RUUID" ] || die "prepare reported no volume"
KARGARGS=""; for k in $KARGS; do KARGARGS="$KARGARGS --karg $k"; done

say "bootc install to-filesystem"
step install "$OUT/03-install.txt" "
podman run --rm --privileged --pid=host --ipc=host \\
  -v /var/lib/containers:/var/lib/containers -v /dev:/dev -v $MNT:/target \\
  --security-opt label=type:unconfined_t localhost/bootc \\
  bootc install to-filesystem --root-mount-spec UUID=$RUUID $KARGARGS \\
  --skip-finalize --target-imgref localhost/tilo:latest /target
" 25 || die "bootc install failed — see $OUT/03-install.txt"

say "enrol factors"
if [ -n "$PASSPHRASE" ]; then
  step enrol "$OUT/04-enrol.txt" "
install -d -m 0700 /run/tilo-install
printf '%s' '$PASSPHRASE' > /run/tilo-install/pin
chmod 0600 /run/tilo-install/pin
/usr/libexec/tilo-disk-encrypt enrol $LUKS --key-file $KEY --pin-file /run/tilo-install/pin
shred -u /run/tilo-install/pin
" 40 || die "enrolment failed — see $OUT/04-enrol.txt. The target has no usable
unlock factor, so do not go on to boot it and conclude anything about early
userspace: it would stop at a prompt no secret can answer."
else
  step enrol "$OUT/04-enrol.txt" \
    "/usr/libexec/tilo-disk-encrypt enrol $LUKS --key-file $KEY" 40 \
    || die "enrolment failed — see $OUT/04-enrol.txt"
fi

# This step, and only this step, prints the recovery key. Keeping a tail of its
# output once discarded it, and with the passphrase slot missing too that left a
# finished target nothing could open — the run had destroyed its own evidence.
RECOVERY_KEY="$(sed -nE 's/^[[:space:]]*([a-z]{4,}(-[a-z]{4,}){3,})[[:space:]]*$/\1/p' \
                  "$OUT/04-enrol.txt" | tr -d '\r' | tail -1)"
if [ -n "$RECOVERY_KEY" ]; then
  printf '%s\n' "$RECOVERY_KEY" > "$OUT/recovery-key.txt"
  echo "recovery key: $RECOVERY_KEY"
  echo "  (saved to $OUT/recovery-key.txt — it is the fallback for this target)"
else
  echo "WARNING: no recovery key found in the enrol output. If the target also has"
  echo "no passphrase slot then nothing will open it; check $OUT/04-enrol.txt."
fi

say "finish"
step finish "$OUT/05-finish.txt" \
  "/usr/libexec/tilo-disk-encrypt finish $LUKS --key-file $KEY --mnt $MNT" 8 \
  || die "finish failed — see $OUT/05-finish.txt"

# Everything the target's own header says about its unlock factors, in full: this
# is the artefact that settles whether enrolment worked, so it is not filtered
# down to a grep.
say "what landed on the target"
step layout "$OUT/06-layout.txt" "
set -u
lsblk -f /dev/vdb
echo '--- luksDump ---'
cryptsetup luksDump $LUKS
mkdir -p /mnt/insp && mount /dev/vdb2 /mnt/insp || exit 1
echo '--- boot entry as installed ---'
cat /mnt/insp/loader/entries/*.conf
echo '--- kernel + initrd present? ---'
find /mnt/insp -maxdepth 3 \\( -name 'vmlinuz*' -o -name 'initramfs*' \\)

# Instrument the installed entry with a serial console. Without this the kernel
# logs to tty0 only, the serial line the harness reads carries the GRUB menu and
# then silence, and a system waiting at its unlock prompt is indistinguishable
# from one that never got that far.
#
# Order is deliberate: ttyS0 first and tty0 LAST, so the kernel and systemd copy
# their log to the serial line while /dev/console — which is where the unlock
# prompt is written and what a typed key reaches — stays the VGA console.
echo '--- adding a serial console to the installed entry ---'
sed -i '/^options /{/console=/!s|\$| console=ttyS0,115200 console=tty0|}' \\
    /mnt/insp/loader/entries/*.conf
grep -q 'console=ttyS0' /mnt/insp/loader/entries/*.conf \\
  || { echo 'FAILED to add the console karg'; umount /mnt/insp; exit 1; }

STRIP_INITRD=$STRIP_INITRD
if [ \"\$STRIP_INITRD\" = 1 ]; then
  echo '--- REMOVING the initrd line to reproduce the pre-initramfs state ---'
  sed -i '/^initrd /d' /mnt/insp/loader/entries/*.conf
  if grep -q '^initrd ' /mnt/insp/loader/entries/*.conf; then
    echo 'FAILED to remove the initrd line'; umount /mnt/insp; exit 1
  fi
  echo 'initrd line removed'
fi
echo '--- boot entry as it will be booted ---'
cat /mnt/insp/loader/entries/*.conf
umount /mnt/insp
" 40 || die "could not inspect or instrument the installed boot entry — see $OUT/06-layout.txt"

if [ "$STRIP_INITRD" = 1 ]; then
  grep -q 'initrd line removed' "$OUT/06-layout.txt" \
    || die "--no-initrd was asked for but the initrd line was not removed; the
run would have proved nothing about the state the image shipped in"
fi

say "stop the installer host, boot the installed disk"
TILO_DISK="$DESKTOP" "$VM" stop
sleep 3
TILO_DISK="$TARGET" "$VM" boot --2d "$TPM_FLAG" || exit 1

# Before concluding anything about the prompt, establish whether the installed
# console is visible at all. GRUB writes to the serial line via firmware, so the
# log is never empty; a kernel marker is what says the instrumentation took.
say "can the installed console be seen?"
OBSERVED=0
for i in $(seq 1 24); do
  sleep 5
  if serial_has 'linux version|command line:|dracut|systemd\[1\]|\[ *ok *\]'; then
    OBSERVED=1; echo "installed console visible on serial after $((i*5))s"; break
  fi
done
[ "$OBSERVED" = 1 ] || echo "CANNOT OBSERVE: no kernel output on the serial line."

PROMPT_SEEN=no
if serial_has 'enter passphrase|passphrase for|please enter'; then PROMPT_SEEN=yes; fi
echo "unlock prompt visible on serial: $PROMPT_SEEN"

if [ -n "$PASSPHRASE" ]; then
  say "type the passphrase at the unlock prompt"
  # The prompt is written to /dev/console, which the karg above deliberately
  # leaves as the VGA console so that a typed key reaches it — so its text need
  # not appear on the serial line, and waiting to see it there before typing
  # would be waiting for something that may never come. Type, then judge by
  # whether the system came up.
  typed=0
  for attempt in $(seq 1 8); do
    if gssh true >/dev/null 2>&1; then
      echo "already unlocked and up (after $typed attempt(s))"; break
    fi
    "$VM" type "$PASSPHRASE" >/dev/null 2>&1
    "$VM" keys ret >/dev/null 2>&1
    typed=$((typed+1))
    echo "  attempt $attempt: passphrase sent"
    sleep 20
  done
fi

# The verdict: did the installed system reach a login? Established by logging
# into it, which is a fact about the system rather than about what the harness
# managed to see.
say "did it unlock and reach a login?"
UNLOCKED=no
if wait_ssh 12; then
  UNLOCKED=yes
  step proof "$OUT/09-proof.txt" '
echo "--- system state ---"
systemctl is-system-running || true
echo "--- what / is on ---"
findmnt -no SOURCE / ; findmnt -no SOURCE /sysroot 2>/dev/null || true
echo "--- block devices ---"
lsblk -f
echo "--- the mapping that is open ---"
for m in /dev/mapper/luks-*; do cryptsetup status "$(basename "$m")" || true; done
echo "--- the header of the volume / is on, as booted ---"
for p in /dev/vda3 /dev/vdb3; do
  cryptsetup isLuks "$p" 2>/dev/null && cryptsetup luksDump "$p"
done
echo "--- login surfaces ---"
systemctl is-active greetd sshd || true
loginctl list-sessions --no-legend 2>/dev/null || true
' 60 || echo "(proof step returned non-zero; see $OUT/09-proof.txt)"
fi

sleep 5
"$VM" serial > "$OUT/07-target-serial.txt" 2>&1 || true
TILO_DISK="$TARGET" "$VM" shot "$LABEL-target" >/dev/null 2>&1 && \
  cp -f "$RUN/$LABEL-target.png" "$OUT/08-target-screen.png" 2>/dev/null || true
TILO_DISK="$TARGET" "$VM" stop

say "verdict for $LABEL"
grep -aiE 'luks|crypt|cannot open root|kernel panic|VFS: Unable|passphrase|reached target|greetd|failed to start|dracut|initramfs|no such device' \
  "$OUT/07-target-serial.txt" | tail -40
echo
echo "unlocked and reached a login : $UNLOCKED"
echo "installed console observable : $([ "$OBSERVED" = 1 ] && echo yes || echo "NO — treat any statement about the prompt as unproven")"
echo "unlock prompt seen on serial : $PROMPT_SEEN"
echo "recovery key                 : ${RECOVERY_KEY:-NONE CAPTURED}"
echo "serial lines: $(wc -l < "$OUT/07-target-serial.txt")   artifacts: $OUT"
[ "$UNLOCKED" = yes ]
