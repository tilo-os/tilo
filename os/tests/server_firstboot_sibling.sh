#!/usr/bin/env bash
# The server's first-boot sibling enrolment, against fakes:
# os/usr/libexec/tilo-server-firstboot spends the install key the installer
# kept inside the encrypted root on `enrol --sibling`, then removes the key's
# LUKS2 slot and shreds it. A failure leaves everything for the next boot; an
# unsafe enrolment file is refused and never handed to cryptsetup.
#
#   bash os/tests/server_firstboot_sibling.sh
#
# Works from the source tree, needs no root and no block device.
set -uo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
script="$repo/os/usr/libexec/tilo-server-firstboot"
fail=0
ok()  { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
bin="$work/bin"
sib="$work/sibling-enrol"
state="$work/state"
dev="$work/volume"
log="$work/calls"
mkdir -p "$bin"
uuid=01234567-89ab-cdef-0123-456789abcdef
uid="$(id -u)"

cat > "$bin/disk-encrypt" <<'EOF'
#!/bin/sh
printf 'enrol %s\n' "$*" >> "$FAKE_LOG"
case "$*" in *--sibling*) ;; *) printf 'disk-encrypt called without --sibling\n' >&2; exit 9 ;; esac
[ "${FAKE_ENROL_RESULT:-ok}" = ok ] || exit 1
EOF
cat > "$bin/auth" <<'EOF'
#!/bin/sh
printf 'auth %s\n' "$*" >> "$FAKE_LOG"
[ "$1 $2" = "sibling status" ] || exit 9
[ "${FAKE_AUTH_STATUS:-none}" = enrolled ] || exit 1
EOF
cat > "$bin/cryptsetup" <<'EOF'
#!/bin/sh
printf 'cryptsetup %s\n' "$*" >> "$FAKE_LOG"
[ "$1" = luksRemoveKey ] || exit 9
[ "${FAKE_REMOVE_RESULT:-ok}" = ok ] || exit 1
EOF
chmod +x "$bin/disk-encrypt" "$bin/auth" "$bin/cryptsetup"

fresh() {
  rm -rf "$sib" "$state"
  install -d -m 0700 "$sib" "$state"
  ( umask 0077; head -c 64 /dev/urandom > "$sib/luks.key" )
  printf 'luks_uuid=%s\nkey_file=%s\n' "$uuid" "$sib/luks.key" > "$sib/enrol.conf"
  chmod 0600 "$sib/enrol.conf"
  : > "$state/sibling-unlock-pending"
  chmod 0644 "$state/sibling-unlock-pending"
  : > "$dev"
  : > "$log"
}

enrol_result=ok
auth_status=none
remove_result=ok
attempt() {
  env PATH="$bin:$PATH" \
    TILO_SERVER_SIBLING_DIR="$sib" TILO_SERVER_STATE_ROOT="$state" \
    TILO_SERVER_SIBLING_OWNER="$uid" \
    TILO_SIBLING_DEVICE="$dev" TILO_SIBLING_ALLOW_REGULAR=1 \
    TILO_TILO_AUTH="$bin/auth" TILO_DISK_ENCRYPT="$bin/disk-encrypt" \
    TILO_CRYPTSETUP="$bin/cryptsetup" TILO_SIBLING_SECONDS=10 \
    FAKE_LOG="$log" FAKE_ENROL_RESULT="$enrol_result" \
    FAKE_AUTH_STATUS="$auth_status" FAKE_REMOVE_RESULT="$remove_result" \
    bash "$script" sibling-enrol > "$work/stdout" 2> "$work/stderr"
  local rc=$?
  enrol_result=ok
  auth_status=none
  remove_result=ok
  return $rc
}

# --- nothing to do without the installer's file -----------------------------
rm -rf "$sib" "$state"
if attempt; then ok "a machine with no kept key does nothing"; else bad "no kept key was not a no-op"; fi
[ ! -s "$log" ] && ok "it calls neither the enroller nor cryptsetup" \
  || { bad "it called something without a kept key"; cat "$log" >&2; }

# --- the happy path ---------------------------------------------------------
fresh
key_before="$(od -An -tx1 "$sib/luks.key" | tr -d ' \n')"
if attempt; then ok "the sibling slot is enrolled"; else
  bad "sibling enrolment failed: $(cat "$work/stderr")"
fi
grep -q -- "--sibling --key-file $sib/luks.key" "$log" \
  && ok "the kept key authorises the sibling enrolment" \
  || { bad "the enrolment did not use the kept key"; cat "$log" >&2; }
grep -q "^cryptsetup luksRemoveKey $dev --key-file $sib/luks.key$" "$log" \
  && ok "the install key's slot is removed" \
  || { bad "the install key slot was left behind"; cat "$log" >&2; }
[ ! -e "$sib/luks.key" ] && [ ! -e "$sib/enrol.conf" ] \
  && ok "the key and its enrolment file are gone" \
  || bad "the key or its enrolment file survived"
[ ! -e "$state/sibling-unlock-pending" ] \
  && ok "the status stops calling the slot pending" \
  || bad "the pending marker survived enrolment"
grep -q -- '--unattended' "$log" && bad "a first boot accepted a boot chain by itself" \
  || ok "the person's device is asked at every boot"
grep -qiF "$key_before" "$work/stdout" "$work/stderr" "$log" \
  && bad "the key's bytes reached a log" \
  || ok "the key's value never reached a log"

# --- enrolment fails: keep it for the next boot -----------------------------
fresh
enrol_result=fail
attempt && ok "a failure to enrol is not a failed boot" \
  || bad "a pending enrolment failed the boot"
[ -f "$sib/luks.key" ] && [ -f "$sib/enrol.conf" ] && [ -e "$state/sibling-unlock-pending" ] \
  && ok "the key, its file and the pending status stay for the next boot" \
  || bad "a pending enrolment dropped what the next boot needs"
grep -q luksRemoveKey "$log" && bad "a pending enrolment removed the install key slot" \
  || ok "a pending enrolment leaves the volume unchanged"
: > "$log"
attempt && ok "the next boot retries" || bad "the retry failed"
grep -q -- '--sibling' "$log" && ok "the retry enrols again" || bad "the retry did not enrol"

# --- already enrolled, the key slot still there -----------------------------
fresh
auth_status=enrolled
attempt && ok "an already-enrolled volume cleans up" \
  || bad "cleanup of an already-enrolled volume failed"
grep -q -- 'disk-encrypt' "$log" && bad "an already-enrolled volume enrolled a second slot" \
  || ok "it does not enrol a second sibling slot"
[ ! -e "$sib/luks.key" ] && [ ! -e "$state/sibling-unlock-pending" ] \
  && ok "it still removes the install key and the pending status" \
  || bad "an already-enrolled volume kept the install key"

# --- removing the slot fails: retry, do not forget what is left -------------
fresh
remove_result=fail
attempt && ok "a failed slot removal is not a failed boot" \
  || bad "a failed slot removal failed the boot"
[ -f "$sib/luks.key" ] && [ -e "$state/sibling-unlock-pending" ] \
  && ok "the key stays so the next boot can remove the slot" \
  || bad "the key was dropped while its slot remained"

# --- unsafe enrolment files are refused, never handed to cryptsetup --------
fresh
chmod 0644 "$sib/enrol.conf"
attempt && bad "a world-readable enrolment file was accepted" \
  || ok "a world-readable enrolment file is refused"
[ -f "$sib/luks.key" ] && ok "a refusal leaves the kept key alone" || bad "a refusal removed the key"

fresh
chmod 0644 "$sib/luks.key"
attempt && bad "a world-readable key was accepted" || ok "a world-readable key is refused"

fresh
printf 'luks_uuid=%s\nkey_file=%s\n' "$uuid" "$work/elsewhere" > "$sib/enrol.conf"
chmod 0600 "$sib/enrol.conf"
attempt && bad "an enrolment file naming another key was accepted" \
  || ok "an enrolment file may only name the kept key"
[ ! -s "$log" ] && ok "a refused file never reaches cryptsetup" \
  || { bad "a refused file reached cryptsetup"; cat "$log" >&2; }

if [ "$fail" -ne 0 ]; then
  printf '\nserver first-boot sibling enrolment: FAILURES\n' >&2
  exit 1
fi
printf '\nserver first-boot sibling enrolment: every check passed\n'
