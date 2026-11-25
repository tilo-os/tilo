#!/usr/bin/env bash
# The server's disk unlock, against fakes: os/usr/libexec/tilo-server-unlock as
# tilo-server-unlock.service runs it. Root adds the key slot and hands the
# request over (prepare), the server's account asks its own daemon (register),
# and root writes the token with the answer or removes the slot again (finish);
# once the volume is set up, the install key's slot is removed and the key
# shredded. A no leaves everything for the next boot; an unsafe install file is
# refused and never handed to cryptsetup.
#
#   bash os/tests/server_unlock.sh
#
# Works from the source tree, needs no root and no block device.
set -uo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
script="$repo/os/usr/libexec/tilo-server-unlock"
unit="$repo/os/usr/lib/systemd/system/tilo-server-unlock.service"
fail=0
ok()  { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
bin="$work/bin"
sib="$work/sibling-enrol"
state="$work/state"
run="$work/run"
dev="$work/volume"
log="$work/calls"
mkdir -p "$bin"
uuid=01234567-89ab-cdef-0123-456789abcdef
uid="$(id -u)"

# tilo-auth: the volume is set up once `finish` wrote a token, which it does only with the
# answer `register` kept; `register` is answered as the person answers.
cat > "$bin/auth" <<'FAKE'
#!/bin/sh
printf 'auth %s\n' "$*" >> "$FAKE_LOG"
[ "$1" = sibling ] || exit 9
case "$2" in
  status) [ -e "$FAKE_TOKEN" ] ;;
  prepare)
    [ "${FAKE_PREPARE:-ok}" = ok ] || exit 1
    while [ "$#" -gt 0 ]; do
      case "$1" in --waiting) waiting="$2"; shift ;; --hand-to) hand="$2"; shift ;; esac
      shift
    done
    : > "$waiting" && printf '{}' > "$hand/asked.json" ;;
  register)
    rm -f "$3/asked.json"
    [ "${FAKE_PERSON:-yes}" = yes ] || { echo "the person said no" >&2; exit 1; }
    printf 'cert' > "$3/answer" ;;
  finish)
    while [ "$#" -gt 0 ]; do
      case "$1" in --waiting) waiting="$2"; shift ;; --answer-in) answer="$2/answer"; shift ;; esac
      shift
    done
    [ -e "$waiting" ] || exit 1
    rm -f "$waiting"
    [ -e "$answer" ] || exit 1
    rm -f "$answer"
    : > "$FAKE_TOKEN" ;;
  *) exit 9 ;;
esac
FAKE
cat > "$bin/cryptsetup" <<'FAKE'
#!/bin/sh
printf 'cryptsetup %s\n' "$*" >> "$FAKE_LOG"
[ "$1" = luksRemoveKey ] || exit 9
[ "${FAKE_REMOVE_RESULT:-ok}" = ok ] || exit 1
FAKE
chmod +x "$bin/auth" "$bin/cryptsetup"

fresh() {
  rm -rf "$sib" "$state" "$run" "$work/token"
  install -d -m 0700 "$sib" "$state" "$run"
  ( umask 0077; head -c 64 /dev/urandom > "$sib/luks.key" )
  printf 'luks_uuid=%s\nkey_file=%s\n' "$uuid" "$sib/luks.key" > "$sib/enrol.conf"
  chmod 0600 "$sib/enrol.conf"
  : > "$state/sibling-unlock-pending"
  chmod 0644 "$state/sibling-unlock-pending"
  : > "$dev"
  : > "$log"
}

prepare_result=ok
person=yes
remove_result=ok
part() {
  env PATH="$bin:$PATH" RUNTIME_DIRECTORY="$run" \
    TILO_SERVER_SIBLING_DIR="$sib" TILO_SERVER_STATE_ROOT="$state" \
    TILO_SERVER_SIBLING_OWNER="$uid" \
    TILO_SIBLING_DEVICE="$dev" TILO_SIBLING_ALLOW_REGULAR=1 \
    TILO_TILO_AUTH="$bin/auth" TILO_CRYPTSETUP="$bin/cryptsetup" \
    FAKE_LOG="$log" FAKE_TOKEN="$work/token" FAKE_PREPARE="$prepare_result" \
    FAKE_PERSON="$person" FAKE_REMOVE_RESULT="$remove_result" \
    bash "$script" "$1" >> "$work/stdout" 2>> "$work/stderr"
}
# What systemd does with the unit: the main part runs only once its start-pre part succeeded, and
# the stop-post part runs whatever they ended in. Answers whether the main part succeeded.
boot() {
  : > "$work/stdout"; : > "$work/stderr"
  local started=1
  if part prepare; then part register && started=0; fi
  part finish || started=2
  prepare_result=ok
  person=yes
  remove_result=ok
  return $started
}

# --- the unit --------------------------------------------------------------
grep -q '^User=tilo-server$' "$unit" && ok "the daemon is asked as its own account" \
  || bad "the unit does not run as tilo-server"
grep -q '^After=tilo-server.service$' "$unit" && ok "it runs once the server's daemon does" \
  || bad "the unit is not ordered after tilo-server.service"
grep -q '^ExecStartPre=+/usr/libexec/tilo-server-unlock prepare$' "$unit" \
  && grep -q '^ExecStart=/usr/libexec/tilo-server-unlock register$' "$unit" \
  && grep -q '^ExecStopPost=+/usr/libexec/tilo-server-unlock finish$' "$unit" \
  && ok "root does the volume's parts, the account its own" \
  || bad "the unit's parts are not root's and the account's"
grep -q '^Type=exec$' "$unit" && ok "no boot waits for the person's answer" \
  || bad "a boot would wait for the person"

# --- nothing to do without the installer's file -----------------------------
rm -rf "$sib" "$state"; install -d -m 0700 "$run"; : > "$log"
if boot; then ok "a machine with no kept key does nothing"; else bad "no kept key was not a no-op"; fi
[ ! -s "$log" ] && ok "it calls neither tilo-auth nor cryptsetup" \
  || { bad "it called something without a kept key"; cat "$log" >&2; }

# --- the person says yes ----------------------------------------------------
fresh
key_before="$(od -An -tx1 "$sib/luks.key" | tr -d ' \n')"
if boot; then ok "the unlock is set up"; else bad "setting up failed: $(cat "$work/stderr")"; fi
grep -q -- "^auth sibling prepare $dev --key-file $sib/luks.key --waiting $sib/waiting.json --hand-to $run$" "$log" \
  && ok "the kept key authorises the slot, and the request goes to the account's directory" \
  || { bad "prepare was not asked as expected"; cat "$log" >&2; }
grep -q "^auth sibling register $run$" "$log" && ok "the account asks its daemon" \
  || { bad "the daemon was not asked"; cat "$log" >&2; }
grep -q "^cryptsetup luksRemoveKey $dev --key-file $sib/luks.key$" "$log" \
  && ok "the install key's slot is removed" \
  || { bad "the install key slot was left behind"; cat "$log" >&2; }
[ ! -e "$sib/luks.key" ] && [ ! -e "$sib/enrol.conf" ] \
  && ok "the key and its file are gone" || bad "the key or its file survived"
[ ! -e "$state/sibling-unlock-pending" ] \
  && ok "the status stops calling the unlock pending" || bad "the pending marker survived"
grep -q -- '--unattended' "$log" && bad "a first boot accepted a boot chain by itself" \
  || ok "the person's device is asked at every boot"
grep -qiF "$key_before" "$work/stdout" "$work/stderr" "$log" \
  && bad "the key's bytes reached a log" || ok "the key's value never reached a log"

# --- the person says no: keep it for the next boot --------------------------
fresh
person=no
boot && bad "a no counted as set up" || ok "a no ends the step without the unlock"
grep -q 'the person said no' "$work/stderr" && ok "the person's words are said" \
  || bad "the refusal's words were lost"
grep -q "^auth sibling finish $dev --key-file $sib/luks.key --waiting $sib/waiting.json --answer-in $run$" "$log" \
  && ok "root settles the slot it added" || { bad "the slot was not settled"; cat "$log" >&2; }
[ ! -e "$sib/waiting.json" ] && ok "nothing waits after a no" || bad "a no left something waiting"
[ -f "$sib/luks.key" ] && [ -f "$sib/enrol.conf" ] && [ -e "$state/sibling-unlock-pending" ] \
  && ok "the key, its file and the pending status stay for the next boot" \
  || bad "a no dropped what the next boot needs"
grep -q luksRemoveKey "$log" && bad "a no removed the install key slot" \
  || ok "a no leaves the volume's install key"
: > "$log"
boot && ok "the next boot asks again, and a yes sets it up" || bad "the retry failed"
[ ! -e "$sib/luks.key" ] && ok "the retry spends the install key" || bad "the retry kept the key"

# --- the slot cannot be added ----------------------------------------------
fresh
prepare_result=fail
boot && bad "a failed slot counted as set up" || ok "a slot that cannot be added fails the step"
grep -q "auth sibling register" "$log" && bad "the daemon was asked without a slot" \
  || ok "no request goes without a slot"
[ -f "$sib/luks.key" ] && ok "the install key stays" || bad "the install key went"

# --- already set up, the install key's slot still there ----------------------
fresh
: > "$work/token"
boot && ok "an already set up volume cleans up" || bad "cleanup of a set up volume failed"
grep -qE 'auth sibling (prepare|register)' "$log" && bad "a set up volume asked again" \
  || ok "it does not ask for a second slot"
[ ! -e "$sib/luks.key" ] && [ ! -e "$state/sibling-unlock-pending" ] \
  && ok "it still removes the install key and the pending status" \
  || bad "a set up volume kept the install key"

# --- removing the slot fails: retry, do not forget what is left -------------
fresh
remove_result=fail
boot && ok "a failed slot removal is not a failed step" || bad "a failed slot removal failed the step"
[ -f "$sib/luks.key" ] && [ -e "$state/sibling-unlock-pending" ] \
  && ok "the key stays so the next boot can remove the slot" \
  || bad "the key was dropped while its slot remained"

# --- unsafe install files are refused, never handed to cryptsetup ------------
fresh
chmod 0644 "$sib/enrol.conf"
boot && bad "a world-readable install file was accepted" || ok "a world-readable install file is refused"
[ -f "$sib/luks.key" ] && ok "a refusal leaves the kept key alone" || bad "a refusal removed the key"

fresh
chmod 0644 "$sib/luks.key"
boot && bad "a world-readable key was accepted" || ok "a world-readable key is refused"

fresh
printf 'luks_uuid=%s\nkey_file=%s\n' "$uuid" "$work/elsewhere" > "$sib/enrol.conf"
chmod 0600 "$sib/enrol.conf"
boot && bad "an install file naming another key was accepted" \
  || ok "an install file may only name the kept key"
[ ! -s "$log" ] && ok "a refused file never reaches tilo-auth or cryptsetup" \
  || { bad "a refused file reached a command"; cat "$log" >&2; }

if [ "$fail" -ne 0 ]; then
  printf '\nserver disk unlock: FAILURES\n' >&2
  exit 1
fi
printf '\nserver disk unlock: every check passed\n'
