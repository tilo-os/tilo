#!/usr/bin/env bash
# A computer's folder keeps syncing whichever holder of its session starts first.
#
# An agent's `tilo mcp` starts the device's holder when there is none; the user service's holder
# starts later (at the next login, or `tilo folders start` restarting it). The service's holder
# takes the device over: the agent's steps down, and the service's holds the session and shows
# the folder. A file written into the folder then reaches the workspace.
#
# Against a running local stack (stack.sh up): TILO_SH_STATE and TILO_SH_PORT_BASE name it, and
# TILO names the tilo program to test. Everything this computer keeps is under a scratch home of
# its own, with no user service manager to reach: nothing of this account's own Tilo, its
# folders or its service is touched.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
stack=$here/../stack.sh
tilo=${TILO:?name the tilo program to test: TILO=target/debug/tilo}
tilo=$(realpath "$tilo")
: "${TILO_SH_STATE:?name the stack: TILO_SH_STATE and TILO_SH_PORT_BASE}"
: "${TILO_SH_PORT_BASE:?name the stack: TILO_SH_STATE and TILO_SH_PORT_BASE}"

scratch=$(mktemp -d "${TMPDIR:-/tmp}/tilo-peer-handover.XXXXXX")
pids=()
finish() {
  local status=$?
  set +e
  for pid in "${pids[@]}"; do kill "$pid" 2>/dev/null || true; done
  for pid in "${pids[@]}"; do wait "$pid" 2>/dev/null || true; done
  # A folder a holder left mounted is let go before its directory goes.
  grep -F " $scratch/home/" /proc/self/mountinfo | awk '{print $5}' | while read -r at; do
    fusermount3 -u -z "$at" 2>/dev/null || true
  done
  if [[ $status != 0 ]]; then
    for log in "$scratch"/*.log; do echo "== $log" >&2; tail -n 40 "$log" >&2 || true; done
  fi
  rm -rf "$scratch"
  exit "$status"
}
trap finish EXIT

eval "$(bash "$stack" env | grep '^export TILO_SITE=\|^export TILO_API_URL=')"
mkdir -p "$scratch/home" "$scratch/state" "$scratch/config" "$scratch/runtime"
chmod 700 "$scratch/runtime"
# Started in the background as an array, not a function, so `$!` is the tilo process itself.
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

say "a person whose identity this computer holds, an owner of the stack"
key=$(as_computer "$tilo" login --new-identity --name Ada --json | python3 -c 'import json,sys; print(json.load(sys.stdin)["user_public_key"])')
bash "$stack" owner "$key" --name Ada >/dev/null
logged_in() { as_computer "$tilo" login >>"$scratch/login.log" 2>&1; }
wait_for 300 "this computer logged in" logged_in
device_dir=$scratch/state/tilo/device
[[ -s $device_dir/key ]] || { say "no device login under $device_dir"; exit 1; }

# The holder `tilo login` left running is stopped: an agent starts the first one below.
peer_pid() {
  as_computer "$tilo" folders --json 2>/dev/null \
    | python3 -c 'import json,sys; print((json.load(sys.stdin).get("peer") or {}).get("pid", ""))'
}
login_holder=$(peer_pid)
[[ -n $login_holder ]] && kill "$login_holder"
wait_for 30 "the login's holder ended" bash -c "! kill -0 '$login_holder' 2>/dev/null"

say "an agent's tilo starts the device's holder"
"${computer[@]}" "$(dirname "$tilo")/tilo-runtime" --hold 2>"$scratch/agent-holder.log" &
agent=$!
pids+=("$agent")
# Asked only while a holder is up: `tilo folders` starts one when none answers.
socket_up() {
  as_computer "$tilo" folders --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin)
sys.exit(0 if d.get("running") and (d.get("peer") or {}).get("mounted") else 1)'
}
wait_for 120 "the agent's holder shows the folder" socket_up

say "the user service's holder starts, and takes the device over"
"${computer[@]}" TILO_PEER_SERVICE=1 "$(dirname "$tilo")/tilo-runtime" --hold 2>"$scratch/service-holder.log" &
service=$!
pids+=("$service")
wait_for 60 "the agent's holder stepped down" bash -c "! kill -0 $agent 2>/dev/null"
kill -0 "$service" || { say "the service's holder ended"; exit 1; }
wait_for 120 "the service's holder shows the folder" socket_up
listed=$(as_computer "$tilo" folders --json)
held_by=$(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["peer"]["pid"])' "$listed")
[[ $held_by == "$service" ]] || { say "the folder is shown by $held_by, not the service's holder $service"; exit 1; }

folder=$(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["peer"]["mounted"][0]["at"])' "$listed")
workspace=$(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["peer"]["mounted"][0]["workspace"])' "$listed")
name="handover-$RANDOM.txt"
say "a file written into $folder reaches the workspace"
printf 'written after the hand-over\n' >"$folder/$name"
in_workspace() { as_computer "$tilo" ls files -w "$workspace" 2>/dev/null | grep -qF "$name"; }
wait_for 60 "$name in the workspace" in_workspace
say "ok: the service's holder took over, and $name reached the workspace"
