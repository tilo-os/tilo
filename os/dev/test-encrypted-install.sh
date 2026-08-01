#!/bin/bash
# Drive an encrypted install inside a booted qOS VM, then boot the result.
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
# guest's own /usr/libexec/qos-disk-encrypt, so what it proves is the shipped
# path and not a re-implementation of it.
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

RUN="${QOS_RUN:-/tmp/qos-vm}"
TARGET="$RUN/$LABEL-target.qcow2"
OUT="${OUT:-$RUN/$LABEL-results}"
mkdir -p "$OUT" "$RUN"
rm -f "$TARGET"
# One TPM state directory per scenario, or a later run unseals against an
# earlier run's TPM and proves nothing about its own enrolment.
export QOS_TPM_DIR="$RUN/tpm-$LABEL"
rm -rf "$QOS_TPM_DIR"

say() { printf '\n### %s\n' "$*"; }

SSHOPTS=(-p "${QOS_SSH_PORT:-2222}" -o StrictHostKeyChecking=no
         -o UserKnownHostsFile=/dev/null -o ConnectTimeout=8 -o LogLevel=ERROR)
GUEST="${QOS_VM_USER:-quickuser}@127.0.0.1"
GUEST_PW="${QOS_VM_PASS:-quickdesktop}"
gssh() { sshpass -p "$GUEST_PW" ssh "${SSHOPTS[@]}" "$GUEST" "$@"; }
# Run a script as root in the guest. The password reaches sudo on stdin and the
# script comes from a file, so the two never contend for stdin.
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

say "boot the installer host ($DESKTOP, $TPM_FLAG)"
QOS_DISK="$DESKTOP" "$VM" boot --2d "$TPM_FLAG" --attach "$TARGET" || exit 1
wait_ssh || { echo "ssh never came up"; "$VM" serial | tail -60; exit 1; }

say "guest facts"
runscript facts '
ls -la /usr/lib/modules/*/initramfs.img 2>&1 || echo "NO INITRAMFS in /usr/lib/modules"
ls /usr/lib/modules
ls /dev/tpm* 2>&1 || echo "no tpm device"
cat /etc/kernel/install.conf 2>&1 || echo "no /etc/kernel/install.conf (expected)"
bash /usr/libexec/qos/boot-chain-test.sh 2>&1 || true
' | tee "$OUT/01-facts.txt" | tail -20

say "copy the booted image into guest container storage"
runscript copy 'bootc image copy-to-storage' | tail -3

say "prepare the encrypted volume"
runscript prepare '/usr/libexec/qos-disk-encrypt prepare /dev/vdb --filesystem btrfs' \
  | tee "$OUT/02-prepare.txt" | tail -20
MNT=$(sed -n 's/^MNT=//p' "$OUT/02-prepare.txt" | tr -d '\r')
LUKS=$(sed -n 's/^LUKS_PART=//p' "$OUT/02-prepare.txt" | tr -d '\r')
KEY=$(sed -n 's/^KEY_FILE=//p' "$OUT/02-prepare.txt" | tr -d '\r')
RUUID=$(sed -n 's/^ROOT_FS_UUID=//p' "$OUT/02-prepare.txt" | tr -d '\r')
KARGS=$(sed -n 's/^KARGS=//p' "$OUT/02-prepare.txt" | tr -d '\r')
[ -n "$MNT" ] && [ -n "$RUUID" ] || { echo "prepare reported no volume"; exit 1; }
KARGARGS=""; for k in $KARGS; do KARGARGS="$KARGARGS --karg $k"; done

say "bootc install to-filesystem"
runscript install "
podman run --rm --privileged --pid=host --ipc=host \\
  -v /var/lib/containers:/var/lib/containers -v /dev:/dev -v $MNT:/target \\
  --security-opt label=type:unconfined_t localhost/bootc \\
  bootc install to-filesystem --root-mount-spec UUID=$RUUID $KARGARGS \\
  --skip-finalize --target-imgref localhost/qos:latest /target
" | tee "$OUT/03-install.txt" | tail -25

say "enrol factors"
if [ -n "$PASSPHRASE" ]; then
  runscript enrol "
install -d -m 0700 /run/qos-install
printf '%s' '$PASSPHRASE' > /run/qos-install/pin
chmod 0600 /run/qos-install/pin
/usr/libexec/qos-disk-encrypt enrol $LUKS --key-file $KEY --pin-file /run/qos-install/pin
shred -u /run/qos-install/pin
" | tee "$OUT/04-enrol.txt" | tail -30
else
  runscript enrol "/usr/libexec/qos-disk-encrypt enrol $LUKS --key-file $KEY" \
    | tee "$OUT/04-enrol.txt" | tail -30
fi

say "finish"
runscript finish "/usr/libexec/qos-disk-encrypt finish $LUKS --key-file $KEY --mnt $MNT" \
  | tee "$OUT/05-finish.txt" | tail -8

say "what landed on the target"
runscript layout "
lsblk -f /dev/vdb
cryptsetup luksDump $LUKS | grep -E '^  [0-9]+: |Tokens:|systemd-|tpm2'
mkdir -p /mnt/insp && mount /dev/vdb2 /mnt/insp && {
  echo '--- boot entry ---'; cat /mnt/insp/loader/entries/*.conf
  echo '--- kernel + initrd present? ---'; find /mnt/insp -maxdepth 3 \( -name 'vmlinuz*' -o -name 'initramfs*' \)
  $( [ "$STRIP_INITRD" = 1 ] && echo \"echo '--- REMOVING the initrd line to reproduce the pre-fix state ---'; sed -i '/^initrd /d' /mnt/insp/loader/entries/*.conf; cat /mnt/insp/loader/entries/*.conf\" )
  umount /mnt/insp; }
" | tee "$OUT/06-layout.txt" | tail -35

say "stop the installer host, boot the installed disk"
QOS_DISK="$DESKTOP" "$VM" stop
sleep 3
QOS_DISK="$TARGET" "$VM" boot --2d "$TPM_FLAG" || exit 1

if [ -n "$PASSPHRASE" ]; then
  asked=0
  for i in $(seq 1 30); do
    sleep 5
    if "$VM" serial 2>/dev/null | grep -aqiE 'enter passphrase|please enter passphrase|passphrase for'; then
      echo "passphrase prompt seen after $((i*5))s; typing it"
      "$VM" type "$PASSPHRASE" >/dev/null 2>&1
      "$VM" keys ret >/dev/null 2>&1
      asked=1; break
    fi
  done
  [ "$asked" = 1 ] || echo "NO passphrase prompt within 150s"
fi
sleep 90
"$VM" serial > "$OUT/07-target-serial.txt" 2>&1 || true
QOS_DISK="$TARGET" "$VM" shot "$LABEL-target" >/dev/null 2>&1 && \
  cp -f "$RUN/$LABEL-target.png" "$OUT/08-target-screen.png" 2>/dev/null || true
QOS_DISK="$TARGET" "$VM" stop

say "verdict for $LABEL"
grep -aiE 'luks|crypt|cannot open root|kernel panic|VFS: Unable|passphrase|reached target|greetd|failed to start|dracut|initramfs|no such device' \
  "$OUT/07-target-serial.txt" | tail -40
echo
echo "serial lines: $(wc -l < "$OUT/07-target-serial.txt")   artifacts: $OUT"
