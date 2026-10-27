#!/usr/bin/env bash
# Assert that `tilo-disk-encrypt enrol` leaves behind a volume its owner can
# actually open. Run against a BUILT image, or from a source tree:
#
#   podman run --rm --entrypoint /bin/bash localhost/tilo:latest \
#       /usr/libexec/tilo/luks-enrolment-test.sh
#
#   DISK_ENCRYPT=os/usr/libexec/tilo-disk-encrypt bash os/tests/luks_enrolment.sh
#
# The volume is a throwaway sparse file, not a block device: everything under
# test here (luksFormat, systemd-cryptenroll, luksAddKey, --test-passphrase)
# works on a file and needs no device-mapper, so this runs in an ordinary
# rootless container.
#
# The supplied secret is always enrolled as a strong LUKS passphrase, whether
# or not the test host exposes a TPM. This prevents a host capability from
# silently changing what the same installer input means.
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

DISK_ENCRYPT="${DISK_ENCRYPT:-/usr/libexec/tilo-disk-encrypt}"
PASSPHRASE="${PASSPHRASE:-correct horse battery staple}"

fail=0
ok()  { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

[ -x "$DISK_ENCRYPT" ] || { bad "no executable tilo-disk-encrypt at $DISK_ENCRYPT"; exit 1; }
for t in cryptsetup systemd-cryptenroll; do
  command -v "$t" >/dev/null 2>&1 || { bad "missing required tool: $t"; exit 1; }
done

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

printf '## universal strong-passphrase path\n'
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

if grep -q 'tilo-startup-secret' <(cryptsetup luksDump "$IMG"); then
  bad "a separate disk passphrase was marked as the account password"
else
  ok "a separate disk passphrase is not marked as the account password"
fi

printf '## the account password as the startup secret (the wizard'"'"'s one secret)\n'
ACCOUNT_IMG="$WORK/account.luks"; ACCOUNT_KEY="$WORK/account.key"
new_volume "$ACCOUNT_IMG" "$ACCOUNT_KEY" || { bad "could not create the account-password test volume"; exit 1; }
if "$DISK_ENCRYPT" enrol "$ACCOUNT_IMG" --key-file "$ACCOUNT_KEY" --pin-file "$WORK/pin" \
     --pin-is-account-password > "$WORK/account.out" 2>&1; then
  ok "enrol --pin-is-account-password exited 0"
else
  bad "enrol --pin-is-account-password exited non-zero:"$'\n'"$(cat "$WORK/account.out")"
fi
# The token names exactly the slot the password opens, so the boot screen
# asks for the password there and for the recovery key in its own slot.
password_slot="$(cryptsetup open --test-passphrase --verbose --key-file "$WORK/pin" "$ACCOUNT_IMG" 2>&1 \
  | sed -nE 's/^Key slot ([0-9]+) unlocked\.$/\1/p')"
marked="$(cryptsetup luksDump "$ACCOUNT_IMG" | awk '
  /^Tokens:/ { tokens = 1; next }
  /^[^ \t]/ { tokens = 0 }
  tokens && /^  [0-9]+: / { ours = ($2 == "tilo-startup-secret") }
  tokens && ours && $1 == "Keyslot:" { print $2 }')"
if [ -n "$password_slot" ] && [ "$marked" = "$password_slot" ]; then
  ok "a tilo-startup-secret token names the account password's slot ($marked)"
else
  bad "expected a tilo-startup-secret token on slot ${password_slot:-?}, found '${marked}'"
fi

printf '## no passphrase (the recovery key is the boot secret)\n'
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
