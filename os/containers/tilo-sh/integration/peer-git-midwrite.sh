#!/usr/bin/env bash
# A git checkout of a large repository into a mounted folder: how many files a second it takes,
# and that the checkout survives its holder ending halfway, however it ends.
#
# The repository is REPO (a git working copy; tilo/tilo itself by default), cloned shallow. Then:
#   - its whole checkout goes into the folder, timed from git's own progress: the files a second
#     over the whole of it and over its first and last quarter (flat in the number of files), and
#     how long `git fsck --full` and `git status` take there;
#   - a checkout of part of it is under way when the holder ends: killed with SIGKILL, asked to
#     step down by a holder of a newer program (a newly installed Tilo, as an agent's `tilo mcp`
#     starts one), and stopped with SIGTERM. A holder asked to step down while files are being
#     written keeps serving until the folder is quiet, so that checkout finishes; after a kill or
#     a stop the folder is shown again, never stale, the checkout done again finishes, and
#     `git fsck --full` finds the repository whole;
#   - a holder with nothing kept of its own reads the whole tree back from the server: the tree
#     there names no chunk it lacks, and the repository checked out first is whole through it.
#
# Against a running local stack (stack.sh up): TILO_SH_STATE and TILO_SH_PORT_BASE name it, and
# TILO names the tilo program to test. What this computer keeps is under a scratch home of its
# own (peer-computer.sh). The figures go to LOGS/throughput.txt. TILO_GIT_MIN_RATE (files a
# second, 1000 by default) is what the whole checkout must reach.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
tilo=${TILO:?name the tilo program to test: TILO=target/debug/tilo}
tilo=$(realpath "$tilo")
repo=${REPO:-$(cd "$here/../../../.." && pwd)}
: "${TILO_SH_STATE:?name the stack: TILO_SH_STATE and TILO_SH_PORT_BASE}"
: "${TILO_SH_PORT_BASE:?name the stack: TILO_SH_STATE and TILO_SH_PORT_BASE}"
min_rate=${TILO_GIT_MIN_RATE:-1000}
# How many files the checkouts the holder ends during write, and how long the whole checkout
# may take before its rate is read from how far it got.
part=${TILO_GIT_PART:-3000}
measure_for=${TILO_GIT_MEASURE_SECS:-900}
# The repository and the clones are owned by whoever made them, not this namespace's root.
export GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=safe.directory GIT_CONFIG_VALUE_0='*'

scratch=$(mktemp -d "${TMPDIR:-/tmp}/tilo-peer-git.XXXXXX")
logs=${TILO_KILL_LOGS:-$scratch}
mkdir -p "$logs"
pids=()
failed=()
finish() {
  local status=$?
  set +e
  for pid in "${pids[@]}"; do kill "$pid" 2>/dev/null || true; done
  pkill -9 -x tilo-runtime 2>/dev/null || true
  unmount_scratch
  if [[ $status != 0 ]]; then
    for log in "$logs"/*.log; do echo "== $log" >&2; tail -n 20 "$log" >&2 || true; done
    declare -F daemon_logs >/dev/null && daemon_logs
    # A failed check keeps no outputs: what the holders warned of goes into its log, each kind
    # once with how often and when first, the numbers in it left out.
    echo "== what the holders warned of" >&2
    find "$scratch/state" "$logs" -name '*.log' -exec grep -h -E ' (WARN|ERROR) ' {} + 2>/dev/null \
      | grep -v 'fuser: \[Not Implemented\]' \
      | python3 -c '
import re, sys
seen = {}
for line in sys.stdin:
    at, _, rest = line.strip().partition(" ")
    key = re.sub(r"[0-9a-f]{8,}|\d+", "N", rest)[:400]
    count, first = seen.get(key, (0, at))
    seen[key] = (count + 1, first)
for key, (count, first) in sorted(seen.items(), key=lambda kv: kv[1][1]):
    print(f"{count:6} {first} {key}")
' >&2 || true
  fi
  rm -rf "$scratch"
  exit "$status"
}
trap finish EXIT
# shellcheck source=peer-computer.sh
source "$here/peer-computer.sh"
fail() { say "FAILED: $*"; failed+=("$*"); }
begun=$SECONDS
step() { say "[$((SECONDS - begun))s] $*"; }
holder_log() { find "$scratch/state" -name '*.log' -path '*/device/*' -size +0 2>/dev/null | head -n 1; }

log_in_and_show
folder=$(mounted at)
say "the holder $(holder_pid) shows $(mounted workspace) at $folder"

say "a shallow clone of $repo to check out from"
src=$scratch/src.git
git clone -q --bare --depth 1 "file://$repo" "$src"
total=$(git -C "$src" ls-tree -r --name-only HEAD | wc -l)
say "its checkout is $total files"

# git's progress through a checkout, as `SECONDS-SINCE-EPOCH DONE TOTAL` lines in FILE.
progress() { # FILE
  python3 -c '
import re, sys, time
out = open(sys.argv[1], "w")
buffer = b""
pattern = re.compile(rb"Updating files:\s+\d+% \((\d+)/(\d+)\)")
while True:
    chunk = sys.stdin.buffer.read1(4096)
    if not chunk:
        break
    buffer += chunk
    *lines, buffer = re.split(rb"[\r\n]", buffer)
    for line in lines:
        m = pattern.search(line)
        if m:
            out.write(f"{time.time():.3f} {int(m[1])} {int(m[2])}\n")
            out.flush()
        elif line.strip():
            sys.stderr.write(line.decode(errors="replace") + "\n")
' "$1"
}
# Clone into DIR without a checkout, keeping only the first N files when N is given.
clone_into() { # DIR [N]
  git clone -q --no-checkout "file://$src" "$1"
  if [[ -n ${2:-} ]]; then
    git -C "$src" ls-tree -r --name-only HEAD >"$scratch/all"
    head -n "$2" "$scratch/all" >"$scratch/part"
    git -C "$1" sparse-checkout set --no-cone --stdin <"$scratch/part"
  fi
}
# Check DIR out, its progress going to FILE: the exit of git.
check_out() { # DIR FILE
  git -C "$1" checkout --progress -f HEAD 2> >(progress "$2") >/dev/null
}
# The files a checkout got through, as its progress last said.
done_of() { [[ -s $1 ]] && tail -n 1 "$1" | awk '{print $2}' || echo 0; }
reached() { # FILE FRACTION
  [[ -s $1 ]] && tail -n 1 "$1" | awk -v f="$2" '{exit !($2 >= f * $3)}'
}
whole() { # DIR WHAT
  local out
  if ! out=$(git -C "$1" fsck --full 2>&1); then
    fail "$2: git fsck --full: $(tail -n 5 <<<"$out")"
    return 1
  fi
  local changed
  changed=$(git -C "$1" status --porcelain 2>&1)
  if [[ -n $changed ]]; then
    fail "$2: git status shows $(wc -l <<<"$changed") changes after the checkout: $(head -n 8 <<<"$changed" | tr '\n' ';')"
    return 1
  fi
}
not_stale() { # WHAT
  if ! stat "$folder" >/dev/null 2>&1 || ! ls "$folder" >/dev/null 2>&1; then
    fail "$1: the folder is stale ($(stat "$folder" 2>&1 | tail -n 1))"
    return 1
  fi
}
# A holder answers again: a status starts one when none is there.
answers_again() {
  as_computer "$tilo" status >/dev/null 2>&1 || true
  shown && [[ -n $(holder_pid) ]]
}

# What the holder is doing when a step stops moving: each thread's stack, twice, a second apart,
# so a wait that never ends shows where it is.
holder_stacks() {
  local holder task
  holder=$(holder_pid)
  [[ -n $holder ]] || return 0
  say "== the holder's threads: name, state, what the kernel has them wait in"
  for task in /proc/"$holder"/task/*; do
    printf '%s %s %s %s\n' "${task##*/}" "$(cat "$task/comm" 2>/dev/null)" \
      "$(awk '/^State:/ {print $2}' "$task/status" 2>/dev/null)" "$(cat "$task/wchan" 2>/dev/null)" >&2
  done
  say "== the stacks of the processes waiting in the folder"
  for task in /proc/[0-9]*; do
    [[ $(awk '/^State:/ {print $2}' "$task/status" 2>/dev/null) == D ]] || continue
    say "${task##*/} $(tr '\0' ' ' <"$task/cmdline" 2>/dev/null | cut -c1-120): $(cat "$task/wchan" 2>/dev/null)"
    cat "$task/stack" 2>/dev/null | head -n 20 >&2 || true
  done
  say "== the end of the holder's log"
  tail -n 40 "$(holder_log)" >&2 2>/dev/null || true
  command -v eu-stack >/dev/null || return 0
  say "== the holder's stacks"
  timeout -s KILL 30 eu-stack -p "$holder" >"$scratch/stuck-stacks.txt" 2>&1 || true
  grep -E '^TID|^#' "$scratch/stuck-stacks.txt" | grep -vE ' (std|core|alloc)::' | head -n 600 >&2 || true
}
# Give a command at most SECONDS: one that takes longer is a stall of the folder, reported
# with the holder's stacks.
bounded() { # SECONDS WHAT COMMAND...
  local limit=$1 what=$2
  shift 2
  "$@" &
  local pid=$! waited=0
  while kill -0 "$pid" 2>/dev/null && ((waited < limit)); do
    sleep 1
    waited=$((waited + 1))
  done
  if kill -0 "$pid" 2>/dev/null; then
    fail "$what did not finish in ${limit}s"
    holder_stacks
    pkill -KILL -P "$pid" 2>/dev/null || true
    kill -KILL "$pid" 2>/dev/null || true
    return 1
  fi
  wait "$pid"
}

step "a clone into the folder, without its checkout"
bounded 300 "the clone into the folder" clone_into "$folder/tilo" || exit 1
step "the whole checkout, timed"
started=$(date +%s.%N)
check_out "$folder/tilo" "$logs/progress-whole.txt" &
checkout=$!
# With TILO_GIT_STACKS=1, where the holder spends a checkout: its threads' stacks sampled for
# twenty seconds, summed by their innermost frames of Tilo's own code. Sampling stops the holder
# each time, so a timed checkout is measured without it.
if [[ ${TILO_GIT_STACKS:-} == 1 ]] && command -v eu-stack >/dev/null; then
  (sleep 20; holder=$(holder_pid)
   for _ in $(seq 80); do echo "=== sample"; eu-stack -p "$holder" 2>/dev/null; sleep 0.25; done >"$scratch/stacks.txt"
   python3 - "$scratch/stacks.txt" >"$logs/stacks.txt" <<'PY'
import collections, re, sys
text = open(sys.argv[1], errors="replace").read()
busy, waits, threads = collections.Counter(), collections.Counter(), collections.Counter()
idle = ("epoll_wait", "park_timeout", "Parker", "Condvar::wait", "read_until", "accept", "nanosleep", "clock_nanosleep", "poll", "sigwait")
for block in re.split(r"\nTID ", text):
    frames = [m.group(1) for m in re.finditer(r"^#\d+\s+0x[0-9a-f]+\s+(.+)$", block, re.M)]
    if not frames:
        continue
    name = block.split("\n", 1)[0]
    ours = [f for f in frames if re.search(r"tilo|automerge|redb|fuser|iroh_blobs|fs_protocol|collab|blake3", f)]
    top = ours[:4]
    head = " | ".join(f[:110] for f in top) or frames[0][:110]
    if any(k in frames[0] for k in ("futex", "syscall")) and not any(k in " ".join(frames[:6]) for k in idle):
        waits[head] += 1
    elif any(k in " ".join(frames[:4]) for k in idle):
        continue
    else:
        busy[head] += 1
print("== busy (thread samples by innermost own frames)")
for k, v in busy.most_common(40): print(f"{v:5} {k}")
print("== blocked in futex/syscall")
for k, v in waits.most_common(40): print(f"{v:5} {k}")
PY
   cat "$logs/stacks.txt" >&2) &
fi
waited=0
while kill -0 "$checkout" 2>/dev/null && ((waited < measure_for)); do
  sleep 1
  waited=$((waited + 1))
  ((waited % 30)) || step "$(done_of "$logs/progress-whole.txt") of $total files checked out"
done
whole_ok=1
if kill -0 "$checkout" 2>/dev/null; then
  pkill -INT -P "$checkout" git 2>/dev/null || true
  kill "$checkout" 2>/dev/null || true
  whole_ok=0
  fail "the whole checkout did not finish in ${measure_for}s ($(done_of "$logs/progress-whole.txt") of $total files)"
  holder_stacks
fi
wait "$checkout" || { ((whole_ok == 0)) || fail "the whole checkout failed"; whole_ok=0; }
ended=$(date +%s.%N)
sleep 0.5 # the progress reader writes git's last lines
rates=$(python3 - "$logs/progress-whole.txt" "$started" "$ended" "$total" <<'EOF'
import sys
log, started, ended, total = sys.argv[1], float(sys.argv[2]), float(sys.argv[3]), int(sys.argv[4])
seen = [tuple(map(float, line.split()[:2])) for line in open(log) if line.strip()]
done = int(seen[-1][1]) if seen else 0
def at(n):
    return next((t for t, d in seen if d >= n), None)
quarter = total // 4
first = at(quarter)
last_from, last_to = at(total - quarter), at(total)
q1 = quarter / (first - started) if first else 0
q4 = quarter / (last_to - last_from) if last_from and last_to and last_to > last_from else 0
print(f"{done} {done / (ended - started):.0f} {q1:.0f} {q4:.0f} {ended - started:.1f}")
EOF
)
read -r got rate first_quarter last_quarter took <<<"$rates"
# What one fsync costs where the holder keeps its state: a closed file's content is made durable
# with one, so it bounds how many files a second a checkout that writes one file at a time gets.
fsync_ms=$(python3 - "$scratch/state" <<'PY'
import os, sys, time
path = os.path.join(sys.argv[1], "fsync-probe")
took = []
with open(path, "wb") as out:
    for _ in range(50):
        out.write(b"x" * 4096)
        started = time.monotonic()
        os.fsync(out.fileno())
        took.append(time.monotonic() - started)
os.remove(path)
took.sort()
print(f"{took[len(took) // 2] * 1000:.2f}")
PY
)
{
  echo "fsync_median_ms: $fsync_ms"
  echo "files: $got of $total in ${took}s"
  echo "files_per_second: $rate"
  echo "first_quarter_files_per_second: $first_quarter"
  echo "last_quarter_files_per_second: $last_quarter"
} | tee "$logs/throughput.txt" >&2
if ((whole_ok)); then
  step "git fsck --full and git status in the whole checkout"
  fsck_started=$SECONDS
  whole "$folder/tilo" "the whole checkout" || true
  echo "fsck_and_status_seconds: $((SECONDS - fsck_started))" | tee -a "$logs/throughput.txt" >&2
fi

# A checkout of part of the repository, under way when END (a function) ends the holder.
during_checkout() { # NAME END
  local name=$1 end=$2 dir=$folder/$1 progress=$logs/progress-$1.txt
  step "== a checkout the holder is ended in: $name"
  clone_into "$dir" "$part"
  check_out "$dir" "$progress" &
  local checkout=$!
  wait_for 600 "$name: the checkout got under way" reached "$progress" 0.3 || true
  "$end" "$checkout"
  local code=0
  wait "$checkout" || code=$?
  echo "$name: git ended with $code after $(done_of "$progress") of $part files" >&2
  echo "$code" >"$scratch/code-$name"
}
redo_and_check() { # NAME
  local name=$1 dir=$folder/$1
  wait_for 180 "$name: a holder shows the folder again" answers_again || { fail "$name: no holder shows the folder again"; return; }
  not_stale "$name" || return
  # A git that died mid-checkout leaves its index lock, as after any crash.
  rm -f "$dir/.git/index.lock"
  git -C "$dir" checkout -q -f HEAD 2>"$logs/redo-$name.log" \
    || { fail "$name: the checkout done again failed: $(tail -n 3 "$logs/redo-$name.log")"; return; }
  whole "$dir" "$name" || true
}

end_by_sigkill() {
  local holder
  holder=$(holder_pid)
  say "killing the holder $holder with SIGKILL"
  kill -9 "$holder"
  wait_for 30 "the holder ended" bash -c "! kill -0 '$holder' 2>/dev/null"
}
during_checkout sigkill end_by_sigkill
redo_and_check sigkill

# A newly installed Tilo is another file, written later: a holder started from it asks the one
# there to step down for it.
mkdir -p "$scratch/newer-install"
newer=$scratch/newer-install/tilo
cp "$tilo" "$newer"
cp "$(dirname "$tilo")/tilo-runtime" "$scratch/newer-install/tilo-runtime"
end_by_newer_program() {
  local holder
  holder=$(holder_pid)
  say "a holder of a newer program asks the holder $holder to step down mid-checkout"
  as_computer "$scratch/newer-install/tilo-runtime" >>"$logs/newer-holder.log" 2>&1 &
  pids+=("$!")
  echo "$holder" >"$scratch/asked-holder"
}
during_checkout newer end_by_newer_program
if [[ $(cat "$scratch/code-newer") != 0 ]]; then
  fail "newer: the checkout failed while a newer program's holder took over (git ended with $(cat "$scratch/code-newer"))"
  redo_and_check newer
else
  not_stale newer || true
  whole "$folder/newer" newer || true
fi
say "once the folder is quiet, the newer program's holder takes over"
took_over() { [[ -n $(holder_pid) && $(holder_pid) != $(cat "$scratch/asked-holder") ]] && shown; }
taken=0
for _ in 1 2 3 4 5 6; do
  as_computer "$scratch/newer-install/tilo-runtime" >>"$logs/newer-holder.log" 2>&1 &
  pids+=("$!")
  if wait_for 40 "the newer program's holder holds the device" took_over; then taken=1; break; fi
done
((taken)) || fail "newer: the newer program's holder did not take over once the folder was quiet"
tilo=$newer

end_by_sigterm() {
  local holder
  holder=$(holder_pid)
  say "stopping the holder $holder with SIGTERM"
  local from=$SECONDS
  kill -TERM "$holder"
  wait_for 40 "the holder stopped" bash -c "! kill -0 '$holder' 2>/dev/null" || fail "sigterm: the holder did not stop within 40s"
  echo "sigterm: the holder stopped in $((SECONDS - from))s" >&2
}
during_checkout sigterm end_by_sigterm
redo_and_check sigterm

say "every way a holder ended said why at error level in its log"
log=$(holder_log)
[[ -n $log ]] && cp "$log" "$logs/holder.log"
for why in "newer" "SIGTERM"; do
  cat "$logs/holder.log" "$logs/newer-holder.log" 2>/dev/null | grep -q "ERROR.*holder ends.*$why" \
    || fail "no holder log has an error-level line for an end by $why"
done

say "a holder with nothing kept of its own reads the whole tree back from the server"
holder=$(holder_pid)
want=$(mounted files)
kill -TERM "$holder"
wait_for 40 "the holder stopped" bash -c "! kill -0 '$holder' 2>/dev/null" || true
kept=$(find "$scratch/state/tilo/device/files" -mindepth 2 -maxdepth 2 -type d -name tree | head -n 1)
[[ -n $kept ]] && rm -rf "$kept" || fail "no kept tree under $scratch/state/tilo/device/files"
wait_for 180 "a fresh holder shows the folder" answers_again || fail "no fresh holder shows the folder"
converged() { synced "$want" any-sent; }
wait_for 600 "the fresh holder's tree names all $want files" converged || fail "the fresh holder's tree is $(short)"
if ((whole_ok)); then
  whole "$folder/tilo" "the whole checkout, read back by a fresh holder" || true
fi

if ((rate < min_rate)); then
  fail "the whole checkout ran at $rate files a second, under $min_rate"
fi
if ((${#failed[@]})); then
  say "${#failed[@]} failed:"
  printf '  %s\n' "${failed[@]}" >&2
  exit 1
fi
say "ok: $total files at $rate files a second; the checkout survived SIGKILL, a newer program and SIGTERM"
