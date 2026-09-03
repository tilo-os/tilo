#!/usr/bin/env bash
# Assert that `qos-disk-encrypt enrol` leaves behind a volume its owner can
# actually open. Run against a BUILT image, or from a source tree:
#
#   podman run --rm --entrypoint /bin/bash localhost/qos:latest \
#       /usr/libexec/qos/luks-enrolment-test.sh
#
#   DISK_ENCRYPT=os/usr/libexec/qos-disk-encrypt bash os/tests/luks_enrolment.sh
#
# The volume is a throwaway sparse file, not a block device: everything under
# test here (luksFormat, systemd-cryptenroll, luksAddKey, --test-passphrase)
# works on a file and needs no device-mapper, so this runs in an ordinary
# rootless container.
#
# A container has no /dev/tpm*, so this exercises the no-TPM path — the one a
# machine without a TPM depends on, where the passphrase the user supplies is
# the only thing standing between them and their data.
#
# The checks exist because `enrol` once added no passphrase key slot at all on
# that path: it captured the recovery key by matching a pattern that did not fit
# what systemd-cryptenroll prints, and under `set -e` the failed match exited
# the command before any further factor was enrolled. The volume was left with a
# single recovery slot whose value had never been shown to anyone, and `--pin`
# was silently a no-op.
#
# Exit codes:
#   0 — every invariant holds.
#   1 — at least one failed; messages go to stderr.

set -uo pipefail

DISK_ENCRYPT="${DISK_ENCRYPT:-/usr/libexec/qos-disk-encrypt}"
PASSPHRASE="${PASSPHRASE:-correct horse battery staple}"

fail=0
ok()  { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

[ -x "$DISK_ENCRYPT" ] || { bad "no executable qos-disk-encrypt at $DISK_ENCRYPT"; exit 1; }
for t in cryptsetup systemd-cryptenroll; do
  command -v "$t" >/dev/null 2>&1 || { bad "missing required tool: $t"; exit 1; }
done

# Every case below is the *no-TPM* branch — `qos-disk-encrypt` picks it by
# `[ -e /dev/tpmrm0 ] || [ -e /dev/tpm0 ]`. On a machine that has a TPM it takes
# the other branch instead and enrols the supplied secret as a TPM2+PIN token,
# which a bare `--key-file` cannot open by design — so the passphrase assertion
# fails and says the fix is broken when the fix is fine and the environment is
# wrong. Refuse rather than mislead. The image build, where this runs, has no
# TPM device; a host may.
if [ -e /dev/tpmrm0 ] || [ -e /dev/tpm0 ]; then
  bad "a TPM is present, so enrol will take the TPM2+PIN branch and these no-TPM
      assertions would test the wrong path. Run this where /dev/tpm* is absent —
      the image build, or a container with those nodes removed."
  exit 1
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# A fresh volume with the throwaway install key in slot 0, standing in for what
# `prepare` hands to `enrol`. pbkdf2 at a low iteration count keeps the check to
# about a second; the KDF is not what is under test.
new_volume() {
  local img="$1" key="$2"
  rm -f "$img"
  truncate -s 32M "$img"
  ( umask 0077; head -c 64 /dev/urandom > "$key" )
  cryptsetup luksFormat --type luks2 --batch-mode \
    --pbkdf pbkdf2 --pbkdf-force-iterations 1000 --key-file "$key" "$img"
}

# Number of populated LUKS2 key slots.
slots_of() {
  cryptsetup luksDump "$1" | grep -cE '^[[:space:]]*[0-9]+: luks2$'
}

# Does $2 (a string) open $1?
opens() {
  local img="$1" secret="$2" f="$WORK/probe"
  ( umask 0077; printf '%s' "$secret" > "$f" )
  local rc=0
  cryptsetup open --test-passphrase --key-file "$f" "$img" 2>/dev/null || rc=1
  rm -f "$f"
  return $rc
}

# Whatever `enrol` prints as the recovery key, taken from its own output the way
# an installer or a user reading the screen would have to take it.
recovery_key_from() {
  sed -nE 's/^[[:space:]]*([a-z]{4,}(-[a-z]{4,}){3,})[[:space:]]*$/\1/p' "$1" | tail -1
}

printf '## no-TPM path, passphrase supplied\n'
IMG="$WORK/pin.luks"; KEY="$WORK/pin.key"
new_volume "$IMG" "$KEY" || { bad "could not create the test volume"; exit 1; }
( umask 0077; printf '%s' "$PASSPHRASE" > "$WORK/pin" )

if "$DISK_ENCRYPT" enrol "$IMG" --key-file "$KEY" --pin-file "$WORK/pin" \
     > "$WORK/pin.out" 2>&1; then
  ok "enrol --pin-file exited 0"
else
  bad "enrol --pin-file exited non-zero:"$'\n'"$(cat "$WORK/pin.out")"
fi

# The check that matters: the secret the user was told to remember opens the
# disk. Everything else in this file is diagnosis for when it does not.
if opens "$IMG" "$PASSPHRASE"; then
  ok "the supplied passphrase opens the volume"
else
  bad "the supplied passphrase does NOT open the volume — the disk is unopenable by the secret its owner was given"
fi

# install key + recovery + passphrase. `finish` removes the install key later,
# so anything less than three here means a factor is missing before it does.
n="$(slots_of "$IMG")"
if [ "${n:-0}" -eq 3 ]; then
  ok "three key slots: install, recovery, passphrase"
else
  bad "expected 3 key slots after enrol, found ${n:-0}"
fi

if grep -q 'systemd-recovery' <(cryptsetup luksDump "$IMG"); then
  ok "a systemd-recovery token is present"
else
  bad "no systemd-recovery token — the recovery factor was not enrolled"
fi

RK="$(recovery_key_from "$WORK/pin.out")"
if [ -n "$RK" ]; then
  ok "enrol printed a recovery key ($(printf '%s' "$RK" | wc -c) chars)"
  # A recovery key that was displayed but does not work is indistinguishable,
  # to its owner, from no recovery key at all.
  if opens "$IMG" "$RK"; then
    ok "the printed recovery key opens the volume"
  else
    bad "the recovery key printed by enrol does NOT open the volume"
  fi
else
  bad "enrol printed no recovery key — the recovery factor cannot be used"
fi

printf '## no-TPM path, no passphrase (the recovery key is the boot secret)\n'
IMG2="$WORK/rec.luks"; KEY2="$WORK/rec.key"
new_volume "$IMG2" "$KEY2" || { bad "could not create the second test volume"; exit 1; }
if "$DISK_ENCRYPT" enrol "$IMG2" --key-file "$KEY2" > "$WORK/rec.out" 2>&1; then
  ok "enrol without --pin-file exited 0"
else
  bad "enrol without --pin-file exited non-zero:"$'\n'"$(cat "$WORK/rec.out")"
fi
RK2="$(recovery_key_from "$WORK/rec.out")"
if [ -n "$RK2" ] && opens "$IMG2" "$RK2"; then
  ok "the printed recovery key opens the volume"
else
  bad "with no passphrase the recovery key is the only way in, and it does not open the volume"
fi

# Enrollment must fail while the throwaway key is still present when the
# recovery value cannot be shown, or when a FIDO2 factor the owner explicitly
# requested cannot be enrolled. Wrapping the real cryptenroll keeps the test on
# a genuine LUKS2 volume while deterministically injecting each boundary fault.
REAL_CRYPTENROLL="$(command -v systemd-cryptenroll)"
mkdir "$WORK/bin"
cat > "$WORK/bin/systemd-cryptenroll" <<'EOF'
#!/bin/bash
set -e
case " $* " in
  *" --recovery-key "*)
    "$REAL_CRYPTENROLL" "$@" >/dev/null
    ;;
  *) exec "$REAL_CRYPTENROLL" "$@" ;;
esac
EOF
chmod 0755 "$WORK/bin/systemd-cryptenroll"

printf '## unreadable recovery value fails closed\n'
IMG4="$WORK/no-recovery-output.luks"; KEY4="$WORK/no-recovery-output.key"
new_volume "$IMG4" "$KEY4" || { bad "could not create the fourth test volume"; exit 1; }
if PATH="$WORK/bin:$PATH" REAL_CRYPTENROLL="$REAL_CRYPTENROLL" \
    "$DISK_ENCRYPT" enrol "$IMG4" --key-file "$KEY4" > "$WORK/no-recovery-output.out" 2>&1; then
  bad "enrol succeeded without returning a recovery key to its owner"
elif grep -q 'could not be read back' "$WORK/no-recovery-output.out"; then
  ok "enrol refuses to finish when the recovery key cannot be shown"
else
  bad "unreadable recovery-key failure did not explain what must be fixed"
fi

cat > "$WORK/bin/systemd-cryptenroll" <<'EOF'
#!/bin/bash
set -e
case " $* " in
  *" --fido2-device=auto "*) exit 23 ;;
  *) exec "$REAL_CRYPTENROLL" "$@" ;;
esac
EOF
chmod 0755 "$WORK/bin/systemd-cryptenroll"

printf '## requested FIDO2 factor fails closed\n'
IMG5="$WORK/fido-failure.luks"; KEY5="$WORK/fido-failure.key"
new_volume "$IMG5" "$KEY5" || { bad "could not create the fifth test volume"; exit 1; }
if PATH="$WORK/bin:$PATH" REAL_CRYPTENROLL="$REAL_CRYPTENROLL" \
    "$DISK_ENCRYPT" enrol "$IMG5" --key-file "$KEY5" --fido2 > "$WORK/fido-failure.out" 2>&1; then
  bad "enrol silently accepted a requested FIDO2 factor that failed"
elif grep -q 'FIDO2 enrolment failed' "$WORK/fido-failure.out"; then
  ok "enrol fails with an actionable error when requested FIDO2 setup fails"
else
  bad "FIDO2 setup failure did not explain what must be fixed"
fi

# `finish` must refuse while the install key is the only other slot, since
# removing it would be what destroys the data.
printf '## finish refuses to strip the last real factor\n'
IMG3="$WORK/bare.luks"; KEY3="$WORK/bare.key"
new_volume "$IMG3" "$KEY3" || { bad "could not create the third test volume"; exit 1; }
if "$DISK_ENCRYPT" finish "$IMG3" --key-file "$KEY3" --mnt "$WORK/nomnt" \
     > "$WORK/bare.out" 2>&1; then
  bad "finish removed the install key from a volume with no other factor"
else
  ok "finish refused on a volume with only the install key"
fi

printf '\n'
if [ "$fail" -eq 0 ]; then
  printf 'all LUKS enrolment invariants hold\n'
else
  printf 'LUKS enrolment invariants FAILED\n' >&2
fi
exit "$fail"
