#!/usr/bin/env bash
# A holder that lives for a long time under a large change stays responsive: `tilo status` and
# `tilo ls` answer within seconds the whole time, none of them says the background sync is busy
# and waits for it, and once the writes stop the tree reaches the workspace and is durable there.
#
# The change goes on for TILO_LONG_MINUTES (default 20): files written, rewritten, renamed and
# removed across many directories, enough that the workspace's tree keeps growing new chunks. The
# holder's CPU time, threads and memory are logged every few seconds, so a cost that grows with
# the holder's age shows up in its logs.
#
# Against a running local stack (stack.sh up): TILO_SH_STATE and TILO_SH_PORT_BASE name it, and
# TILO names the tilo program to test. Everything this computer keeps is under a scratch home of
# its own, with no user service manager to reach: nothing of this account's own Tilo, its
# folders or its service is touched.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
tilo=${TILO:?name the tilo program to test: TILO=target/debug/tilo}
tilo=$(realpath "$tilo")
: "${TILO_SH_STATE:?name the stack: TILO_SH_STATE and TILO_SH_PORT_BASE}"
: "${TILO_SH_PORT_BASE:?name the stack: TILO_SH_STATE and TILO_SH_PORT_BASE}"
minutes=${TILO_LONG_MINUTES:-20}
# The longest a status or a listing may take, in seconds.
answer_within=${TILO_LONG_ANSWER_WITHIN:-10}
# The minute of the change the holder is killed at with SIGKILL, as a computer's service is when it
# does not stop in time, and the next one started by a status; 0 keeps the first holder throughout.
kill_at_minute=${TILO_LONG_KILL_AT_MINUTE:-12}

scratch=$(mktemp -d "${TMPDIR:-/tmp}/tilo-peer-long.XXXXXX")
logs=${TILO_LONG_LOGS:-$scratch}
mkdir -p "$logs"
pids=()
holder=
finish() {
  local status=$?
  set +e
  for pid in "${pids[@]}"; do kill "$pid" 2>/dev/null || true; done
  for pid in "${pids[@]}"; do wait "$pid" 2>/dev/null || true; done
  [[ -z $holder ]] || kill -9 "$holder" 2>/dev/null || true
  unmount_scratch
  # The holder's own log, whatever the outcome: what it did while it was asked.
  find "$scratch" -name '*.log' -type f -exec cp {} "$logs/" \; 2>/dev/null || true
  # What the device keeps on disk, largest last: how much of the tree's history it holds.
  du -ab "$scratch" 2>/dev/null | sort -n | tail -n 30 >"$logs/kept-sizes.txt" || true
  if [[ $status != 0 ]]; then
    for log in "$logs"/*.log; do echo "== $log" >&2; tail -n 40 "$log" >&2 || true; done
  fi
  rm -rf "$scratch"
  exit "$status"
}
trap finish EXIT

source "$here/peer-computer.sh"

log_in_and_show
folder=$(mounted at)
holder=$(holder_pid)
[[ -n $holder ]] || { say "no holder answers after the login"; exit 1; }
say "the holder $holder shows $(mounted workspace) at $folder"

# Each status and each listing: when it was asked, how long it took, its exit, and whether it
# said the sync was busy.
timed() { # NAME COMMAND...
  local name=$1 began ended said code busy
  shift
  began=$(date +%s.%N)
  said=$(timeout 120 "${computer[@]}" "$@" 2>&1) && code=0 || code=$?
  ended=$(date +%s.%N)
  busy=0
  if grep -qF "background sync is" <<<"$said"; then busy=1; fi
  printf '%s %s %s %s %s\n' "$began" "$name" "$(python3 -c "print(round($ended - $began, 2))")" \
    "$code" "$busy" >>"$logs/asked.log"
  if [[ $code != 0 || $busy == 1 ]]; then
    printf '== %s %s (%s)\n%s\n' "$began" "$name" "$code" "$said" >>"$logs/asked-said.log"
  fi
}
asking() {
  while :; do
    timed status "$tilo" status
    timed ls "$tilo" ls
    sleep 1
  done
}
asking &
pids+=("$!")

# The holder's age, CPU seconds, threads and resident memory, every few seconds.
watching() { # PID
  local ticks
  ticks=$(getconf CLK_TCK)
  while kill -0 "$1" 2>/dev/null; do
    awk -v t="$ticks" -v now="$(date +%s)" -v pid="$1" \
      '{print now, ($14 + $15) / t, $20, $24 * 4096, pid}' \
      "/proc/$1/stat" >>"$logs/holder.log" 2>/dev/null || true
    sleep 5
  done
}
watching "$holder" &
pids+=("$!")

# The holder killed and the next one started, between two rounds of the change: when, and by whom.
restart_holder() {
  local first=$holder
  say "killing the holder $first with SIGKILL"
  echo "$(date +%s.%N) killed" >>"$logs/restart.log"
  kill -9 "$first"
  wait_for 30 "the holder $first ended" bash -c "! kill -0 '$first' 2>/dev/null"
  wait_for 180 "a new holder shows the folder" shown
  holder=$(holder_pid)
  [[ -n $holder && $holder != "$first" ]] || { say "no new holder after the kill (saw '$holder')"; exit 1; }
  echo "$(date +%s.%N) shown" >>"$logs/restart.log"
  say "the holder $holder shows the folder again"
  watching "$holder" &
  pids+=("$!")
}

# The change: a round writes a batch of files into a directory of their own, rewrites some of
# the last round's, renames some and removes others, for as long as the change lasts.
changing() {
  local round=0 began=$SECONDS until=$((SECONDS + minutes * 60)) dir last killed=0
  while ((SECONDS < until)); do
    if ((kill_at_minute > 0 && !killed && SECONDS - began >= kill_at_minute * 60)); then
      restart_holder
      killed=1
    fi
    dir=$folder/long/r$((round / 20))/b$round
    mkdir -p "$dir"
    for ((i = 0; i < 50; i++)); do
      printf 'round %d file %d\n%0256d\n' "$round" "$i" "$i" >"$dir/f$i.txt"
    done
    if ((round > 0)); then
      last=$folder/long/r$(((round - 1) / 20))/b$((round - 1))
      for ((i = 0; i < 10; i++)); do printf 'again %d\n' "$round" >>"$last/f$i.txt"; done
      for ((i = 10; i < 15; i++)); do mv "$last/f$i.txt" "$last/moved-$i.txt"; done
      rm -f "$last"/f2[0-4].txt
    fi
    echo "$round $(date +%s)" >>"$logs/rounds.log"
    round=$((round + 1))
  done
  echo "$round" >"$scratch/rounds"
}
say "changing $folder/long for $minutes minutes"
changing 2>>"$logs/writer.log"
rounds=$(cat "$scratch/rounds")
# Each round leaves 45 files: 50 written, 5 removed in the next (the last round's stay whole).
want=$((rounds * 45 + 5))
say "$rounds rounds written; the tree names all $want files and is durable on the server"
converged() { synced "$want"; }
wait_for 600 "the tree names all $want files and is durable on the server" converged \
  || { say "$(short)"; exit 1; }
kill "${pids[0]}" 2>/dev/null || true
wait "${pids[0]}" 2>/dev/null || true

say "every status and listing answered within ${answer_within}s, and none said the sync was busy"
touch "$logs/restart.log"
python3 - "$logs/asked.log" "$logs/holder.log" "$answer_within" "$logs/restart.log" <<'EOF'
import sys
asked, holder, within, restart = sys.argv[1], sys.argv[2], float(sys.argv[3]), sys.argv[4]
marks = {what: float(at) for at, what in (line.split() for line in open(restart) if line.strip())}
# While a killed holder's successor starts, nothing answers: those asks are not counted.
gap = (marks["killed"] - 1, marks["shown"] + 30) if "killed" in marks and "shown" in marks else None
rows = [line.split() for line in open(asked) if line.strip()]
rows = [r for r in rows if gap is None or float(r[0]) + float(r[2]) < gap[0] or float(r[0]) > gap[1]]
if len(rows) < 20:
    sys.exit(f"only {len(rows)} statuses and listings were asked")
failed = 0
for name in ("status", "ls"):
    mine = [r for r in rows if r[1] == name]
    took = sorted(float(r[2]) for r in mine)
    slow = [r for r in mine if float(r[2]) > within]
    bad = [r for r in mine if r[3] != "0"]
    busy = [r for r in mine if r[4] == "1"]
    print(f"{name}: {len(mine)} asked, median {took[len(took) // 2]}s, slowest {took[-1]}s, "
          f"{len(slow)} over {within}s, {len(bad)} failed, {len(busy)} said busy")
    failed += len(slow) + len(bad) + len(busy)
lines = [line.split() for line in open(holder) if line.strip()]
for pid in dict.fromkeys(line[4] for line in lines):
    mine = [line for line in lines if line[4] == pid]
    if len(mine) >= 2:
        (t0, c0, n0, m0, _), (t1, c1, n1, m1, _) = mine[0], mine[-1]
        span = max(float(t1) - float(t0), 1)
        print(f"holder {pid}: {span:.0f}s, {float(c1) - float(c0):.0f} CPU seconds, threads {n0} -> {n1}, "
              f"memory {int(m0) >> 20} -> {int(m1) >> 20} MiB")
sys.exit(1 if failed else 0)
EOF
kill -0 "$holder" || { say "the holder $holder ended"; exit 1; }
say "every part of the file list read here, across the restart"
unreadable=$(find "$scratch" -name '*.log' -type f -exec grep -h "cannot be read here" {} + 2>/dev/null || true)
[[ -z $unreadable ]] || { say "parts of the file list did not read: $(head -n 3 <<<"$unreadable")"; exit 1; }
say "ok: $rounds rounds over $minutes minutes; status and listings answered throughout"
