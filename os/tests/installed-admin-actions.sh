#!/usr/bin/env bash
# Drive the logged-in installed desktop as its device admin, through the action
# model alone: no TILO_TEST_CONTROL, no synthetic input, no screenshot. Run over
# the admin's own ssh login after first login is complete.
#
#   1. the compositor serves the admin control socket by default;
#   2. any Notes window already open closes through its own action;
#   3. invoking Notes' launcher action, with no listing before it, succeeds;
#   4. `tilo-control actions list` answers, and lists Notes;
#   5. the state read (`tilo-control windows`) shows its new window;
#   6. a note created through Notes' new-page action reads back, as typed
#      data, through Notes' read action;
#   7. the Notes it opened closes again through its own action, pass or fail,
#      so the checks after it start from the desktop they expect.
set -euo pipefail

fail() { printf 'admin actions: %s\n' "$*" >&2; exit 1; }

[[ -z ${TILO_TEST_CONTROL:-} ]] || fail "TILO_TEST_CONTROL is set; this check proves the path without it"
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
shell_pid=$(pgrep -u "$(id -u)" -x shell | head -n 1) || fail "no compositor process"
if shell_env=$(tr '\0' '\n' 2>/dev/null <"/proc/$shell_pid/environ"); then
  ! grep -q '^TILO_TEST_CONTROL=' <<<"$shell_env" \
    || fail "the compositor runs with TILO_TEST_CONTROL; this check must not depend on it"
else
  test_socket="$XDG_RUNTIME_DIR/tilo-session/test-control.sock"
  [[ ! -S $test_socket ]] \
    || fail "the compositor serves $test_socket; this check must not depend on it"
fi

notes_tabs() {
  tilo-control windows --json \
    | python3 -c 'import json,sys; [print("%s:%s" % (w["group"], w["tab"])) for w in json.load(sys.stdin) if w.get("app_id") == "so.tilo.notes"]'
}

# Close every Notes window through its own action and wait until it is gone.
close_notes() {
  local open_tabs tab
  open_tabs=$(notes_tabs) || { echo "windows read failed" >&2; return 1; }
  for tab in $open_tabs; do
    tilo-control actions invoke desktop-windows "tab.close:$tab" >/dev/null || {
      echo "closing Notes tab $tab through desktop-windows:tab.close:$tab failed" >&2
      return 1
    }
  done
  for _ in $(seq 1 30); do
    [[ -z $(notes_tabs) ]] && return 0
    sleep 1
  done
  echo "Notes tabs still open 30 s after closing them: $(notes_tabs)" >&2
  return 1
}

# Earlier checks may have left Notes open, and opening an open app only
# focuses it. Close those windows first, so the launch below must produce a
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

# Invoke by address first: an address resolves without a listing before it.
answer=$(tilo-control actions invoke applications so.tilo.notes) \
  || fail "invoking applications:so.tilo.notes failed"
printf 'invoke answered: %s\n' "$answer"
python3 - "$answer" <<'PY' || fail "the invocation did not report success"
import json, sys
answer = json.loads(sys.argv[1])
update = answer["receipt"]["update"]
assert update.get("state") in ("succeeded", "accepted"), update
PY

listed=$(tilo-control actions list notes --json) || fail "actions list failed"
python3 - "$listed" <<'PY' || fail "actions list does not offer applications:so.tilo.notes"
import json, sys
rows = json.loads(sys.argv[1])["actions"]
assert any(r["provider"] == "applications" and r["id"] == "so.tilo.notes" for r in rows), rows
PY

opened=""
for _ in $(seq 1 60); do
  opened=$(notes_tabs) || fail "windows read failed"
  [[ -n $opened ]] && break
  sleep 1
done
if [[ -z $opened ]]; then
  tilo-control windows >&2 || true
  fail "no Notes window 60 s after the action succeeded"
fi
tilo-control windows
echo "admin actions: Notes opened through the action model"

# Write through Notes' own action, then read back through its `read`: content
# is reached as typed data, never from a screenshot.
title="Admin check $(date +%s)"
new_page=""
for _ in $(seq 1 30); do
  rows=$(tilo-control actions list new note --json) || fail "actions list failed"
  new_page=$(python3 -c 'import json,sys
rows = json.loads(sys.argv[1])["actions"]
print(next((r["id"] for r in rows if r["provider"] == "app-actions"
            and r.get("application") == "so.tilo.notes" and r.get("operation") == "new-page"), ""))' "$rows")
  [[ -n $new_page ]] && break
  sleep 1
done
[[ -n $new_page ]] || fail "Notes publishes no new-page action 30 s after it opened"
args=$(python3 -c 'import json,sys; print(json.dumps({"value": sys.argv[1]}))' "$title")
tilo-control actions invoke app-actions "$new_page" --args-json "$args" >/dev/null \
  || fail "creating a note through new-page failed"

item=""
for _ in $(seq 1 30); do
  notebook=$(tilo-control read so.tilo.notes --json) || fail "reading the notebook failed"
  item=$(python3 -c 'import json,sys
pages = json.loads(sys.argv[1]).get("pages", [])
print(next((p["item"] for p in pages if p["title"] == sys.argv[2]), ""))' "$notebook" "$title")
  [[ -n $item ]] && break
  sleep 1
done
[[ -n $item ]] || fail "the notebook read does not list \"$title\": $notebook"
note=$(tilo-control read so.tilo.notes "$item" --json) || fail "reading $item failed"
python3 -c 'import json,sys
note = json.loads(sys.argv[1])
assert note["kind"] == "notes.page" and note["title"] == sys.argv[2], note
assert isinstance(note["blocks"], list), note' "$note" "$title" \
  || fail "reading $item did not answer the note: $note"
echo "admin actions: a note written through new-page reads back through read"
