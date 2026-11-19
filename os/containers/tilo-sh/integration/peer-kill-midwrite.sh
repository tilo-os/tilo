#!/usr/bin/env bash
# A folder's bulk write survives its holder being killed halfway, and `tilo status` asked all the
# while never replaces a holder that is there.
#
# Thousands of files go into a mounted folder, enough that the workspace's tree grows new
# chunks while they land. Halfway the holder is killed with SIGKILL: it gets no last step,
# no flush, no unmount. `tilo status` and `tilo folders` are asked over and over the whole time.
# Then:
#   - every holder a status saw stayed alive until this script killed it: no command replaced
#     one, whatever it answered;
#   - after the kill a status starts the next holder (nothing was there), and every file the
#     folder acknowledged before the kill is still in it;
#   - the writer finishes, and the workspace's tree names every file and is durable on the
#     server;
#   - a holder that knows nothing of its own (its kept tree removed) reads the whole tree back
#     from the server: the tree there names no chunk it lacks.
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
# How many files the write makes, and after how many the holder is killed.
count=${TILO_KILL_FILES:-3000}
kill_at=${TILO_KILL_AT:-$((count / 2))}

scratch=$(mktemp -d "${TMPDIR:-/tmp}/tilo-peer-kill.XXXXXX")
logs=${TILO_KILL_LOGS:-$scratch}
mkdir -p "$logs"
pids=()
holders=()
finish() {
  local status=$?
  set +e
  for pid in "${pids[@]}"; do kill "$pid" 2>/dev/null || true; done
  for pid in "${pids[@]}"; do wait "$pid" 2>/dev/null || true; done
  for pid in "${holders[@]}"; do kill -9 "$pid" 2>/dev/null || true; done
  grep -F " $scratch/home/" /proc/self/mountinfo | awk '{print $5}' | while read -r at; do
    fusermount3 -u -z "$at" 2>/dev/null || true
  done
  if [[ $status != 0 ]]; then
    for log in "$logs"/*.log; do echo "== $log" >&2; tail -n 40 "$log" >&2 || true; done
  fi
  rm -rf "$scratch"
  exit "$status"
}
trap finish EXIT

eval "$(bash "$stack" env | grep '^export TILO_SITE=\|^export TILO_API_URL=')"
mkdir -p "$scratch/home" "$scratch/state" "$scratch/config" "$scratch/runtime"
chmod 700 "$scratch/runtime"
computer=(env -u DBUS_SESSION_BUS_ADDRESS -u TILO_API_KEY -u TILO_DEVICE_DIR
  HOME="$scratch/home" XDG_STATE_HOME="$scratch/state" XDG_CONFIG_HOME="$scratch/config"
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

say "a person whose identity this computer holds, an owner of the stack"
key=$(as_computer "$tilo" login --new-identity --name Ada --json | python3 -c 'import json,sys; print(json.load(sys.stdin)["user_public_key"])')
bash "$stack" owner "$key" --name Ada >/dev/null
logged_in() { as_computer "$tilo" login >>"$logs/login.log" 2>&1; }
wait_for 300 "this computer logged in" logged_in
wait_for 120 "the holder shows the folder" shown
folder=$(mounted at)
workspace=$(mounted workspace)
first=$(holder_pid)
holders+=("$first")
[[ -n $first ]] || { say "no holder answers after the login"; exit 1; }
say "the holder $first shows $workspace at $folder"

# Every status, its exit and the holder it reached, one line each, the whole time.
asking() {
  while :; do
    local said pid
    said=$(as_computer "$tilo" status 2>&1) && code=0 || code=$?
    pid=$(as_computer "$tilo" folders --json 2>/dev/null \
      | python3 -c 'import json,sys; print((json.load(sys.stdin).get("peer") or {}).get("pid", "-"))' 2>/dev/null || echo -)
    printf '%s %s %s\n' "$(date +%s.%N)" "$code" "$pid" >>"$logs/asked.log"
    if grep -qF 'No workspace has a folder here yet' <<<"$said"; then
      date +%s.%N >>"$logs/no-folder.log"
    fi
  done
}
asking &
pids+=("$!")

# The writer: each file is written until the folder acknowledges it (its close succeeded), and
# then named in acknowledged.log. A file the folder refused while no holder was there is written
# again once one is.
mkdir -p "$scratch/blob"
head -c 1048576 /dev/urandom >"$scratch/blob/mib"
writing() {
  local i dir
  for ((i = 0; i < count; i++)); do
    dir=$folder/bulk/d$((i / 200))
    until mkdir -p "$dir" 2>/dev/null \
      && if ((i % 500 == 7)); then cp "$scratch/blob/mib" "$dir/f$i.bin"; else
        printf 'file %d of a bulk write\n%0512d\n' "$i" "$i" >"$dir/f$i.txt"; fi 2>/dev/null; do
      sleep 0.2
    done
    echo "$i" >>"$logs/acknowledged.log"
  done
  echo done >"$scratch/written"
}
say "writing $count files into $folder/bulk"
writing 2>>"$logs/writer.log" &
writer=$!
pids+=("$writer")

acknowledged() { [[ -s $logs/acknowledged.log ]] && (($(wc -l <"$logs/acknowledged.log") >= kill_at)); }
wait_for 600 "$kill_at files acknowledged" acknowledged
before=$(wc -l <"$logs/acknowledged.log")
say "killing the holder $first with SIGKILL after $before files"
kill -9 "$first"
killed_at=$(date +%s.%N)
wait_for 30 "the holder $first ended" bash -c "! kill -0 '$first' 2>/dev/null"

say "a status starts the next holder, which shows the folder again"
wait_for 180 "a new holder shows the folder" shown
second=$(holder_pid)
holders+=("$second")
shown_again_at=$(date +%s.%N)
[[ -n $second && $second != "$first" ]] || { say "no new holder after the kill (saw '$second')"; exit 1; }

say "every file acknowledged before the kill is in the folder"
lost=0
while read -r i; do
  dir=$folder/bulk/d$((i / 200))
  [[ -e $dir/f$i.txt || -e $dir/f$i.bin ]] || { lost=$((lost + 1)); say "lost: f$i"; }
done < <(head -n "$before" "$logs/acknowledged.log")
((lost == 0)) || { say "$lost files the folder acknowledged before the kill are gone"; exit 1; }

say "the write finishes"
wait_for 900 "the writer finished" test -e "$scratch/written"
want=$count
files_in_folder() { (($(find "$folder/bulk" -type f | wc -l) == want)); }
wait_for 120 "all $want files in the folder" files_in_folder
converged() {
  [[ $(mounted files) -ge $want && $(mounted sent) == true && $(mounted durable) == true ]]
}
wait_for 600 "the tree names all $want files and is durable on the server" converged
kill "${pids[0]}" 2>/dev/null || true
wait "${pids[0]}" 2>/dev/null || true

say "no status replaced a holder that was there"
python3 - "$logs/asked.log" "$first" "$second" "$killed_at" <<'EOF'
import sys
log, first, second, killed_at = sys.argv[1], sys.argv[2], sys.argv[3], float(sys.argv[4])
seen = [line.split() for line in open(log) if line.strip()]
if len(seen) < 5:
    sys.exit(f"only {len(seen)} statuses were asked")
failed = 0
for at, code, pid in seen:
    at = float(at)
    if pid == "-":
        continue
    if at < killed_at and pid != first:
        print(f"before the kill a status reached holder {pid}, not {first}"); failed += 1
    if at > killed_at + 1 and pid not in (first, second):
        print(f"after the kill a status reached holder {pid}, not {second}"); failed += 1
after = [pid for at, _, pid in seen if float(at) > killed_at + 1 and pid != "-"]
if second not in after:
    print("no status after the kill reached the new holder"); failed += 1
if after and after[-1] != second:
    print(f"the last status reached {after[-1]}, not {second}"); failed += 1
print(f"{len(seen)} statuses asked, {sum(1 for s in seen if s[1] != '0')} not ok")
sys.exit(1 if failed else 0)
EOF
kill -0 "$second" || { say "the holder $second ended"; exit 1; }
# Between the kill and the next holder's mount nothing shows the folder; before and after, a
# status that says there is no folder here is the tree read as empty.
if [[ -s $logs/no-folder.log ]]; then
  python3 - "$logs/no-folder.log" "$killed_at" "$shown_again_at" <<'EOF'
import sys
log, killed, shown = sys.argv[1], float(sys.argv[2]), float(sys.argv[3])
wrong = [float(at) for at in open(log) if at.strip() and not killed <= float(at) <= shown + 2]
if wrong:
    sys.exit(f"{len(wrong)} statuses said no workspace has a folder here while one was shown")
EOF
fi

say "a holder with nothing kept of its own reads the whole tree back from the server"
kill -TERM "$second"
stopped_in=$SECONDS
wait_for 40 "the holder $second stopped within its stop timeout" bash -c "! kill -0 '$second' 2>/dev/null"
say "the holder stopped in $((SECONDS - stopped_in))s after SIGTERM"
kept=$(find "$scratch/state/tilo/device/files" -mindepth 2 -maxdepth 2 -type d -name tree | head -n 1)
[[ -n $kept ]] || { say "no kept tree under $scratch/state/tilo/device/files"; exit 1; }
rm -rf "$kept"
wait_for 180 "a fresh holder shows the folder" shown
wait_for 600 "the fresh holder's tree names all $want files" converged
wait_for 120 "all $want files in the fresh holder's folder" files_in_folder
say "ok: $want files written across a SIGKILL after $before; no status replaced a holder; the server's tree reads whole"
