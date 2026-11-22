#!/usr/bin/env bash
# Drive the logged-in installed desktop as its device admin, through the action
# model alone and the commands a person types: no TILO_TEST_CONTROL, no
# synthetic input, no screenshot. Run over the admin's own ssh login after
# first login is complete.
#
#   1. the compositor serves the admin control socket by default;
#   2. any Notes window already open closes through its own action;
#   3. `tilo open notes` succeeds;
#   4. `tilo action list notes` answers, and offers Notes' new note;
#   5. the state read (`tilo-control windows`) shows its new window;
#   6. a note made through `tilo do notes#new-note` answers as typed data,
#      and is then offered under its title;
#   7. the Notes it opened closes again through its own action, pass or fail,
#      so the checks after it start from the desktop they expect.
set -euo pipefail

fail() { printf 'admin actions: %s\n' "$*" >&2; exit 1; }

[[ -z ${TILO_TEST_CONTROL:-} ]] || fail "TILO_TEST_CONTROL is set; this check proves the path without it"
command -v tilo >/dev/null || fail "tilo is not installed in the image"
command -v tilo-control >/dev/null || fail "tilo-control is not installed in the image"
command -v python3 >/dev/null || fail "python3 is required"

export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
socket="$XDG_RUNTIME_DIR/tilo-session/admin-control.sock"

# The socket appears once the session has finished first login.
for _ in $(seq 1 60); do
  [[ -S $socket ]] && break
  sleep 1
done
[[ -S $socket ]] || fail "no admin control socket at $socket 60 s after login"
mode=$(stat -c %a "$socket")
[[ $mode == 600 ]] || fail "admin control socket is mode $mode, expected 600"

# The compositor must not have been started with the dev control variable.
# Its environment is unreadable when the process is not dumpable; then the
# variable's effect is checked instead: no test-control socket is served.
shell_pid=$(pgrep -u "$(id -u)" -x tilo-session | head -n 1) || fail "no session process"
if shell_env=$(tr '\0' '\n' 2>/dev/null <"/proc/$shell_pid/environ"); then
  ! grep -q '^TILO_TEST_CONTROL=' <<<"$shell_env" \
    || fail "the compositor runs with TILO_TEST_CONTROL; this check must not depend on it"
else
  test_socket="$XDG_RUNTIME_DIR/tilo-session/test-control.sock"
  [[ ! -S $test_socket ]] \
    || fail "the compositor serves $test_socket; this check must not depend on it"
fi

# Each Notes window, by the resource its actions act on.
notes_windows() {
  tilo-control windows --json \
    | python3 -c 'import json,sys; [print(w.get("resource") or "desktop:tab:%s" % w["tab"]) for w in json.load(sys.stdin) if w.get("app_id") == "so.tilo.notes"]'
}

# Close every Notes window through its own action and wait until it is gone.
close_notes() {
  local open_windows window
  open_windows=$(notes_windows) || { echo "windows read failed" >&2; return 1; }
  for window in $open_windows; do
    tilo do "$window#close-tab" >/dev/null || {
      echo "closing Notes window $window through $window#close-tab failed" >&2
      return 1
    }
  done
  for _ in $(seq 1 30); do
    [[ -z $(notes_windows) ]] && return 0
    sleep 1
  done
  echo "Notes windows still open 30 s after closing them: $(notes_windows)" >&2
  return 1
}

# Earlier checks may have left Notes open, and opening an open app only
# focuses it. Close those windows first, so the open below must produce a
# new one.
close_notes || fail "Notes was open before the check and did not close"

# The checks after this one open apps from an empty workspace: leave it so,
# whether this check passes or fails.
cleanup() {
  local status=$?
  if ! close_notes; then
    printf 'admin actions: the Notes this check opened did not close\n' >&2
    ((status != 0)) || status=1
  fi
  exit "$status"
}
trap cleanup EXIT

tilo open notes || fail "tilo open notes failed"

opened=""
for _ in $(seq 1 60); do
  opened=$(notes_windows) || fail "windows read failed"
  [[ -n $opened ]] && break
  sleep 1
done
if [[ -z $opened ]]; then
  tilo-control windows >&2 || true
  fail "no Notes window 60 s after it opened"
fi
tilo-control windows
echo "admin actions: Notes opened through the action model"

listed=""
for _ in $(seq 1 30); do
  listed=$(tilo action list new note) || fail "tilo action list failed"
  grep -qi 'new note' <<<"$listed" && break
  listed=""
  sleep 1
done
[[ -n $listed ]] || fail "Notes offers no new note 30 s after it opened"

# Write through Notes' own action: its answer is typed data, and the note it
# made is then offered under its title, never read from a screenshot.
title="Admin check $(date +%s)"
made=$(tilo do notes#new-note "title=$title" --json) \
  || fail "making a note through notes#new-note failed"
python3 -c 'import json,sys
answer = json.loads(sys.argv[1])
assert answer["status"] == "ok", answer' "$made" \
  || fail "notes#new-note did not answer as typed data: $made"
found=""
for _ in $(seq 1 30); do
  found=$(tilo action list "$title") || fail "tilo action list failed"
  grep -qF "$title" <<<"$found" && break
  found=""
  sleep 1
done
[[ -n $found ]] || fail "nothing is offered on the note \"$title\" 30 s after it was made"
echo "admin actions: a note made through new-note is offered under its title"
