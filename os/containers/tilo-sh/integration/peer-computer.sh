# shellcheck shell=bash
# A scratch computer for the peer's checks: a home, state, config and runtime directory of its
# own, with no user service manager to reach, so nothing of this account's own Tilo, its folders
# or its service is touched. Sourced by peer-kill-midwrite.sh and peer-git-midwrite.sh, after they
# set `tilo` (the program to test), `scratch` (a new directory) and `logs`, against a running
# local stack (stack.sh up) that TILO_SH_STATE and TILO_SH_PORT_BASE name.

eval "$(bash "$here/../stack.sh" env | grep '^export TILO_SITE=\|^export TILO_API_URL=')"
mkdir -p "$scratch/home" "$scratch/state" "$scratch/config" "$scratch/runtime"
chmod 700 "$scratch/runtime"
computer=(env -u DBUS_SESSION_BUS_ADDRESS -u TILO_API_KEY
  HOME="$scratch/home" XDG_STATE_HOME="$scratch/state" XDG_CONFIG_HOME="$scratch/config"
  TILO_IDENTITY_ROOT="$scratch/identity"
  XDG_RUNTIME_DIR="$scratch/runtime" TILO_SITE="$TILO_SITE" TILO_API_URL="$TILO_API_URL")
as_computer() { "${computer[@]}" "$@"; }
say() { printf '%s\n' "$*" >&2; }
wait_for() { # SECONDS WHAT COMMAND...
  local until=$((SECONDS + $1)) what=$2
  shift 2
  until "$@"; do
    ((SECONDS < until)) || { say "timed out: $what"; return 1; }
    sleep 0.5
  done
}
# The holder's process id as `tilo folders` reaches it.
holder_pid() {
  as_computer "$tilo" folders --json 2>/dev/null \
    | python3 -c 'import json,sys; print((json.load(sys.stdin).get("peer") or {}).get("pid", ""))'
}
# The folder this computer shows, as `tilo folders` reports it: KEY of its first mount.
mounted() {
  as_computer "$tilo" folders --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin)
m = ((d.get("peer") or {}).get("mounted") or [])
if not m:
    sys.exit(1)
v = m[0].get(sys.argv[1])
print("" if v is None else (str(v).lower() if isinstance(v, bool) else v))' "$1"
}
shown() { mounted at >/dev/null; }
# Log in as a new person who owns the stack, and wait for the holder to show the folder.
log_in_and_show() {
  say "a person whose identity this computer holds, an owner of the stack"
  local key
  key=$(as_computer "$tilo" login --new-identity --name Ada --json | python3 -c 'import json,sys; print(json.load(sys.stdin)["user_public_key"])')
  bash "$here/../stack.sh" owner "$key" --name Ada >/dev/null
  logged_in() { as_computer "$tilo" login >>"$logs/login.log" 2>&1; }
  wait_for 300 "this computer logged in" logged_in
  wait_for 120 "the holder shows the folder" shown
}
# Unmount whatever the scratch home still has mounted.
unmount_scratch() {
  grep -F " $scratch/home/" /proc/self/mountinfo | awk '{print $5}' | while read -r at; do
    fusermount3 -u -z "$at" 2>/dev/null || true
  done
}
