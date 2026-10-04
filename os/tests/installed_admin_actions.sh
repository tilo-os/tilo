#!/usr/bin/env bash
# Drive the logged-in installed desktop as its device admin, through the action
# model alone: no TILO_TEST_CONTROL, no synthetic input, no screenshot. Run over
# the admin's own ssh login after first login is complete.
#
#   1. the compositor serves the admin control socket by default;
#   2. any Notes window already open closes through its own action;
#   3. invoking Notes' launcher action, with no listing before it, succeeds;
#   4. `tilo-control actions list` answers, and lists Notes;
#   5. the state read (`tilo-control windows`) shows its new window.
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
shell_pid=$(pgrep -u "$(id -u)" -x shell | head -n 1) || fail "no compositor process"
if tr '\0' '\n' <"/proc/$shell_pid/environ" | grep -q '^TILO_TEST_CONTROL='; then
  fail "the compositor runs with TILO_TEST_CONTROL; this check must not depend on it"
fi

notes_tabs() {
  tilo-control windows --json \
    | python3 -c 'import json,sys; [print("%s:%s" % (w["group"], w["tab"])) for w in json.load(sys.stdin) if w.get("app_id") == "so.tilo.notes"]'
}

# Earlier checks may have left Notes open, and opening an open app only
# focuses it. Close those windows through their own actions first, so the
# launch below must produce a new one.
open_tabs=$(notes_tabs) || fail "windows read failed"
for tab in $open_tabs; do
  tilo-control actions invoke desktop-windows "tab.close:$tab" >/dev/null \
    || fail "closing Notes tab $tab through desktop-windows:tab.close:$tab failed"
done
for _ in $(seq 1 30); do
  [[ -z $(notes_tabs) ]] && break
  sleep 1
done
[[ -z $(notes_tabs) ]] || fail "Notes tabs still open 30 s after closing them: $(notes_tabs)"

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

for _ in $(seq 1 60); do
  opened=$(notes_tabs) || fail "windows read failed"
  if [[ -n $opened ]]; then
    tilo-control windows
    echo "admin actions: Notes opened through the action model"
    exit 0
  fi
  sleep 1
done
tilo-control windows >&2 || true
fail "no Notes window 60 s after the action succeeded"
