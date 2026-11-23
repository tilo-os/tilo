# shellcheck shell=bash
# A scratch computer for the peer's checks: a home, state, config and runtime directory of its
# own, and a second home for the person's own computer, which holds their root and adds this one,
# with no user service manager to reach, so nothing of this account's own Tilo, its folders or its
# service is touched. Sourced by peer-kill-midwrite.sh, peer-git-midwrite.sh, peer-long-change.sh
# and peer-handover.sh, after they set `tilo` (the program to test), `scratch` (a new directory)
# and `logs`, against a running local stack (stack.sh up) that TILO_SH_STATE and TILO_SH_PORT_BASE
# name.

eval "$(bash "$here/../stack.sh" env | grep '^export TILO_SITE=\|^export TILO_API_URL=')"
mkdir -p "$scratch/home" "$scratch/state" "$scratch/config" "$scratch/runtime"
chmod 700 "$scratch/runtime"
mkdir -p "$scratch/person/home" "$scratch/person/state" "$scratch/person/config" \
  "$scratch/person/runtime"
chmod 700 "$scratch/person/runtime"
person=(env -u DBUS_SESSION_BUS_ADDRESS -u TILO_API_KEY
  HOME="$scratch/person/home" XDG_STATE_HOME="$scratch/person/state"
  XDG_CONFIG_HOME="$scratch/person/config" TILO_IDENTITY_ROOT="$scratch/person/identity"
  XDG_RUNTIME_DIR="$scratch/person/runtime" TILO_SITE="$TILO_SITE" TILO_API_URL="$TILO_API_URL")
computer=(env -u DBUS_SESSION_BUS_ADDRESS -u TILO_API_KEY
  HOME="$scratch/home" XDG_STATE_HOME="$scratch/state"
  XDG_CONFIG_HOME="$scratch/config" TILO_IDENTITY_ROOT="$scratch/identity"
  XDG_RUNTIME_DIR="$scratch/runtime" TILO_SITE="$TILO_SITE" TILO_API_URL="$TILO_API_URL")
as_person() { "${person[@]}" "$@"; }
as_computer() { "${computer[@]}" "$@"; }
# The end of each computer's daemon log (`tilo-runtime`, which mounts the folders): what a
# check that failed prints beside its own logs.
daemon_logs() {
  local log
  for log in "$scratch"/state/tilo/device/*.log "$scratch"/person/state/tilo/device/*.log; do
    [[ -f $log ]] || continue
    echo "== $log" >&2
    tail -n 60 "$log" >&2 || true
  done
}
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
# Whether the first mount names at least WANT files and is durable on the server, and, unless the
# second argument is `any-sent`, every change made here went out. Each record that differs from the
# last is logged with its time in folder.log, so a wait that runs out shows when the folder
# stopped moving, and `short` names what it still lacks.
synced() { # WANT [any-sent]
  as_computer "$tilo" folders --json 2>/dev/null | python3 -c '
import json, sys, time
want, sent, log = int(sys.argv[1]), sys.argv[2] != "any-sent", sys.argv[3]
m = ((json.load(sys.stdin).get("peer") or {}).get("mounted") or [{}])[0]
keys = ("files", "sent", "durable", "opened", "state", "stuck", "host_lacks", "connected")
record = json.dumps({k: m.get(k) for k in keys}, sort_keys=True)
try:
    last = open(log).read().splitlines()[-1].split(" ", 1)[1]
except (OSError, IndexError):
    last = None
if record != last:
    with open(log, "a") as out:
        out.write(f"{time.time():.1f} {record}\n")
files = m.get("files") or 0
short = [f"files {files} of {want}"] if files < want else []
short += [k for k in (("sent", "durable") if sent else ("durable",)) if m.get(k) is not True]
open(log + ".short", "w").write(", ".join(short))
sys.exit(1 if short else 0)' "$1" "${2:-}" "$logs/folder.log"
}
# What the folder lacked at the last `synced`, and the record it stood at.
short() { printf 'short of %s; the folder stood at %s' "$(cat "$logs/folder.log.short" 2>/dev/null)" "$(tail -n 1 "$logs/folder.log" 2>/dev/null)"; }
# Log in as a new person who owns the stack, and wait for the holder to show the folder.
#
# The person's own computer holds their root. It joins the workspace the stack lets them in to by
# the invitation its owner sends, then adds this computer with a ticket, as `tilo device add` adds
# any computer of theirs: the ticket is kept served until this one has logged in with it.
log_in_and_show() {
  say "a person whose own computer holds their identity, an owner of the stack"
  local key link ticket add
  key=$(as_person "$tilo" login --new-identity --name Ada --json | python3 -c 'import json,sys; print(json.load(sys.stdin)["user_public_key"])')
  bash "$here/../stack.sh" owner "$key" --name Ada >/dev/null
  link=$(bash "$here/../stack.sh" invite -- --role administrator)
  as_person "$tilo" login "$link" >>"$logs/person-login.log" 2>&1
  as_person "$tilo" device add --json >>"$logs/device-add.log" 2>&1 &
  add=$!
  wait_for 60 "the person's computer printed a ticket" test -s "$logs/device-add.log"
  ticket=$(python3 -c 'import json,sys; print(json.loads(open(sys.argv[1]).readline())["ticket"])' "$logs/device-add.log")
  as_computer "$tilo" login "$ticket" >>"$logs/login.log" 2>&1 \
    || { kill "$add" 2>/dev/null || true; return 1; }
  wait "$add" || say "the person's computer did not finish adding this one"
  wait_for 120 "the holder shows the folder" shown || {
    say "what this computer says of its folders:"
    as_computer "$tilo" folders >&2 || true
    as_computer "$tilo" status >&2 || true
    return 1
  }
  # Both computers mount their folders from their daemon's first start: a daemon that ended
  # says why on a line of its own in its log, and one started again after it is the fault. A
  # second daemon that found the device kept by the first ends too, and is no fault.
  local ended
  ended=$(cat "$scratch"/state/tilo/device/*.log "$scratch"/person/state/tilo/device/*.log \
    2>/dev/null | grep '^tilo-runtime: ' | grep -vc 'another daemon keeps it' || true)
  ((ended == 0)) || { say "a daemon ended $ended times before the folder showed"; return 1; }
}
# Unmount whatever the scratch homes still have mounted.
unmount_scratch() {
  grep -F " $scratch/" /proc/self/mountinfo | awk '{print $5}' | while read -r at; do
    fusermount3 -u -z "$at" 2>/dev/null || true
  done
}
