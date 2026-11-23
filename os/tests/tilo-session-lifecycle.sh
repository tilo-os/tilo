#!/usr/bin/env bash
# Source/runtime contract for the greetd -> systemd user target -> verified
# identity -> compositor lifecycle. Uses a rewritten temporary copy so the
# production wrapper keeps absolute, non-overridable trusted command paths.
set -euo pipefail
# Most assertions are bare greps: name the one that failed instead of
# exiting silently. Commands run under `set +e` fail on purpose and are
# checked by status, so only an errexit failure is reported.
trap 'if [[ $- == *e* ]]; then echo "tilo-session-lifecycle: line $LINENO failed: $BASH_COMMAND" >&2; fi' ERR

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
. "$repo/tools/desktop-isolation.sh"
. "$repo/dist/session/tests/build-flight-recorder.sh"
tilo_flight_recorder_build "$repo"
tilo_test_isolate "$@"
unset GNOME_KEYRING_CONTROL
wrapper="$repo/os/usr/libexec/tilo-session"
target="$repo/os/usr/lib/systemd/user/tilo-graphical-session.target"
selinux_policy="$repo/os/usr/share/tilo/selinux/tilo-base.cil"
identity_unit="$repo/os/usr/lib/systemd/user/tilo-identity.service"
session_main="$repo/crates/desktop/session/src/main.rs"

grep -Fqx 'readonly DBUS_UPDATE=/usr/bin/dbus-update-activation-environment' "$wrapper"
grep -Fqx '"$DBUS_UPDATE" --systemd XDG_CURRENT_DESKTOP XDG_SESSION_TYPE GLIBC_TUNABLES' "$wrapper"
grep -Fqx '"$SYSTEMCTL" --user import-environment XDG_CURRENT_DESKTOP XDG_SESSION_TYPE GLIBC_TUNABLES' "$wrapper"
# The PAM-started keyring daemon's control socket reaches D-Bus activation, so
# no second keyring daemon is spawned.
grep -Fqx '  "$DBUS_UPDATE" --systemd GNOME_KEYRING_CONTROL' "$wrapper"

grep -Fqx 'Requires=graphical-session-pre.target graphical-session.target' "$target"
grep -Fqx 'After=graphical-session-pre.target' "$target"
grep -Fqx 'PartOf=graphical-session.target' "$target"
grep -Fqx 'PropagatesStopTo=graphical-session.target graphical-session-pre.target' "$target"
grep -Fqx 'Wants=tilo-ush.service' "$target"
for socket in \
  "$repo/os/usr/lib/systemd/user/tilo-models.socket" \
  "$repo/os/usr/lib/systemd/user/tilo-model-gateway.socket" \
  "$repo/os/usr/lib/systemd/user/tilo-projector.socket"; do
  grep -Fqx 'PartOf=tilo-graphical-session.target' "$socket"
  grep -Fqx 'WantedBy=tilo-graphical-session.target' "$socket"
done
grep -Fqx 'ulimit -c 0' "$wrapper"
# The session runs in the user's SELinux domain (pam_selinux in
# os/etc/pam.d/greetd), never greetd's xdm_t, so the policy grants the display
# manager nothing a user session needs.
if grep -Eq '\(allow xdm_t (tilo_session_unit_file_t|home_root_t)' "$selinux_policy"; then
  echo 'the policy lets the display manager domain run the user session' >&2
  exit 1
fi
# The compositor verifies and repairs an existing identity on every start;
# there is no separate pre-session identity unit to order anything after.
if [ -e "$identity_unit" ]; then
  echo 'a pre-session identity unit is back' >&2
  exit 1
fi
# The identity helper must return before the long-lived session services bind:
# the process verifies or repairs this device's identity, then starts the shell.
# GPU selection intentionally happens elsewhere so every process shape uses the
# same explicit device policy; line-order checks against an older startup
# layout made this lifecycle test fail without proving a runtime defect.
identity_line=$(grep -n 'tilo_session::startup::identity()' "$session_main" | cut -d: -f1)
shell_start_line=$(grep -n '^    start(ours, command, line,' "$session_main" | cut -d: -f1)
if [ -z "$identity_line" ] || [ -z "$shell_start_line" ] \
   || [ "$identity_line" -ge "$shell_start_line" ]; then
  echo 'identity helper no longer returns before the session services start' >&2
  exit 1
fi
# The production sandbox holds everywhere, dev images included: a tilo-vm
# slot's user manager runs these units exactly as a login does.
for unit in tilo-fs-cloud.service tilo-models.service; do
  prod="$repo/os/usr/lib/systemd/user/$unit"
  for kept in \
    'ProtectSystem=strict' \
    'ProtectHome=read-only' \
    'ProtectKernelTunables=yes' \
    'ProtectKernelModules=yes' \
    'ProtectKernelLogs=yes' \
    'PrivateTmp=yes' \
    'ProtectControlGroups=yes' \
    'ProtectClock=yes' \
    'ProtectHostname=yes'; do
    grep -Fqx "$kept" "$prod" || {
      echo "$unit lost production sandbox directive $kept" >&2
      exit 1
    }
  done
  if grep -Eq '^PrivateIPC=' "$prod"; then
    grep -Fqx 'PrivateIPC=yes' "$prod" || {
      echo "$unit production PrivateIPC is no longer yes" >&2
      exit 1
    }
  fi
  if ! grep -Eq '^ReadWritePaths=.+' "$prod"; then
    echo "$unit lost production ReadWritePaths" >&2
    exit 1
  fi
done
if [ -e "$repo/os/dev-image/usr/lib/systemd/user" ]; then
  echo 'the dev image ships user-unit drop-ins that change the production sandbox' >&2
  exit 1
fi
grep -Fqx 'Name=Tilo' \
  "$repo/os/usr/share/wayland-sessions/tilo-session.desktop"
grep -Fqx 'X-Tilo-Session-Default=true' \
  "$repo/os/usr/share/wayland-sessions/tilo-session.desktop"
# A login with no flag runs the native session, and the image offers no second
# one.
grep -Fqx 'Exec=/usr/libexec/tilo-session --backend drm' \
  "$repo/os/usr/share/wayland-sessions/tilo-session.desktop"
[ "$(ls "$repo/os/usr/share/wayland-sessions" | tr '\n' ' ')" = 'tilo-session.desktop ' ] || {
  echo 'the image must ship exactly one login session' >&2
  exit 1
}
# The composed image is refused with a session or X11 bridge that has an unresolved library
# (os-image-contracts runs the programs contract on it, after every package is in).
contract="$repo/os/tests/image-programs-contract.sh"
grep -Fq 'missing="$(ldd /usr/bin/tilo-session /usr/bin/xwayland-satellite | awk '\''/not found/ { print $1 }'\'')"' \
  "$contract"
grep -Fq 'if [ -n "$missing" ]; then' "$contract"
grep -Fq 'the session or xwayland-satellite has unresolved runtime libraries:' "$contract"
grep -Fq 'os/tests/image-programs-contract.sh' "$repo/t.toml"
grep -Fq 'the browser engine has unresolved runtime libraries:' "$repo/os/Containerfile"

tmp_root="${TMPDIR:-/tmp}"
tmp=$(mktemp -d "$tmp_root/tilo-session-lifecycle.XXXXXX")
cleanup() {
  case "$tmp" in "$tmp_root"/tilo-session-lifecycle.*) rm -r "$tmp" ;; esac
}
trap cleanup EXIT INT TERM

log="$tmp/log"
fake_systemctl="$tmp/systemctl"
fake_dbus_update="$tmp/dbus-update-activation-environment"
fake_compositor="$tmp/tilo-session-bin"
fake_native="$tmp/native-session"
test_wrapper="$tmp/tilo-session"
runtime_dir="$tmp/runtime"
install -d -m 700 "$runtime_dir"

cat >"$fake_systemctl" <<'SH'
#!/bin/bash
printf 'systemctl %s\n' "$*" >>"$TILO_SESSION_TEST_LOG"
exit 0
SH
cat >"$fake_dbus_update" <<'SH'
#!/bin/bash
printf 'dbus-update %s\n' "$*" >>"$TILO_SESSION_TEST_LOG"
exit 0
SH
cat >"$fake_compositor" <<'SH'
#!/bin/bash
# Both compositor binaries run this one body; its name is what tells them
# apart in the log.
name=$(basename "$0")
if [ "${1:-}" = --verify-collaboration-identity ]; then
  printf 'verify\n' >>"$TILO_SESSION_TEST_LOG"
  count_file="$TILO_SESSION_TEST_LOG.verify-count"
  count=0
  [ ! -f "$count_file" ] || count=$(cat "$count_file")
  count=$((count + 1))
  printf '%s\n' "$count" >"$count_file"
  sequence="${TILO_SESSION_VERIFY_SEQUENCE:-0}"
  status=$(printf '%s\n' "$sequence" | cut -d, -f"$count")
  [ -n "$status" ] || status=$(printf '%s\n' "$sequence" | awk -F, '{print $NF}')
  exit "$status"
fi
if [ "${1:-}" = --provision-identity ]; then
  printf 'provision\n' >>"$TILO_SESSION_TEST_LOG"
  exit "${TILO_SESSION_PROVISION_STATUS:-0}"
fi
# Exercise the wrapper's path contract with a create_new writer, like shell.
if [ -n "${TILO_FRAME_CPU_TRACE:-}" ]; then
  python3 - "$TILO_FRAME_CPU_TRACE" <<'PYTHON'
import os, sys
path = sys.argv[1]
fd = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
os.close(fd)
assert os.stat(os.path.dirname(path)).st_mode & 0o777 == 0o700
assert os.stat(path).st_mode & 0o777 == 0o600
PYTHON
fi
printf '%s\n' "${TILO_FRAME_CPU_TRACE:-disabled}" >>"$TILO_SESSION_TEST_LOG.cpu-paths"
printf '%s %s\n' "$name" "$*" >>"$TILO_SESSION_TEST_LOG"
runs_file="$TILO_SESSION_TEST_LOG.runs"
runs=0
[ ! -f "$runs_file" ] || runs=$(cat "$runs_file")
runs=$((runs + 1))
printf '%s\n' "$runs" >"$runs_file"
# The wrapper announces an in-place restart in the login's private channel before it asks the
# compositor to stop. The session reads that marker once at start and removes it; this stand-in
# does the same, recording what each run saw and where the channel is.
marker="$(dirname "$TILO_SESSION_LOGOUT_REQUEST")/restart-request"
printf '%s\n' "$TILO_SESSION_LOGOUT_REQUEST" >"$TILO_SESSION_TEST_LOG.request-path"
printf 'restart-marker=%s\n' "$([ -e "$marker" ] && printf yes || printf no)" >>"$TILO_SESSION_TEST_LOG.markers"
rm -f -- "$marker"
# Observe the real wrapper's recovery channel before simulating a compositor
# that hands its locked state back on ordered teardown.
recovery_file="$(dirname "$TILO_SESSION_LOGOUT_REQUEST")/lock-state"
cat "$recovery_file" >>"$TILO_SESSION_TEST_LOG.recovery"
[ "$runs" != 1 ] || printf 'locked\n' >"$recovery_file"
printf 'control=%s\nfirst-use=%s\n' "${TILO_TEST_CONTROL:-}" "${TILO_TEST_CONTROL_FIRST_USE:-}" \
  >"$TILO_SESSION_TEST_LOG.control"
# The login's handover for an in-place restart: the wrapper opens it once, every compositor run
# inherits it, and the run that follows writes the byte the previous run's keeper waits on.
printf 'handover=%s\n' "${TILO_SESSION_RESTART_HANDOVER:-disabled}" >"$TILO_SESSION_TEST_LOG.handover"
if [ -n "${TILO_SESSION_RESTART_HANDOVER:-}" ] \
   && printf x >&"$TILO_SESSION_RESTART_HANDOVER" 2>/dev/null; then
  printf 'handover-writable=yes\n' >>"$TILO_SESSION_TEST_LOG.handover"
else
  printf 'handover-writable=no\n' >>"$TILO_SESSION_TEST_LOG.handover"
fi
printf 'state=%s\ndata=%s\nconfig=%s\n' "${XDG_STATE_HOME:-}" "${XDG_DATA_HOME:-}" "${XDG_CONFIG_HOME:-}" \
  >"$TILO_SESSION_TEST_LOG.xdg"
printf '%s-output %s trace=%s\n' "$name" "$*" "${TILO_FRAME_TRACE:-}"
if [ "${TILO_SESSION_WRITE_LOGOUT:-0}" = 1 ]; then
  case "${TILO_SESSION_LOGOUT_REQUEST:-}" in
    /*) ;;
    *)
      printf 'logout channel is not absolute\n' >&2
      exit 42
      ;;
  esac
  printf '%s\n' "$TILO_SESSION_LOGOUT_REQUEST" >"$TILO_SESSION_TEST_LOG.request-path"
  printf 'logout\n' >"$TILO_SESSION_LOGOUT_REQUEST"
fi
# A desktop a person can use says so in its ready record, which names its process.
if [ "${TILO_SESSION_PUBLISH_READY:-0}" = 1 ] && [ -d "$XDG_RUNTIME_DIR/tilo-session" ]; then
  printf '%s 0\n' "$$" >"$XDG_RUNTIME_DIR/tilo-session/desktop-ready"
fi
if [ "${TILO_SESSION_HOLD:-0}" = 1 ] \
   || { [ "${TILO_SESSION_HOLD_FIRST:-0}" = 1 ] && [ "$runs" = 1 ]; }; then
  # A test that stops the login while the compositor is being stopped needs the compositor to
  # take a moment, as a real one does.
  trap 'printf "%s-term\n" "$name" >>"$TILO_SESSION_TEST_LOG"; sleep "${TILO_SESSION_TERM_DELAY:-0}"; exit 0' TERM HUP INT
  printf '%s\n' "$runs" >"$TILO_SESSION_TEST_LOG.holding"
  while :; do sleep 1; done
fi
if [ "${TILO_SESSION_IGNORE_TERM:-0}" = 1 ]; then
  # A compositor stuck so badly that it cannot end its own login: every stop signal is
  # ignored, as a loop parked with its ending flag unread would be.
  trap '' TERM HUP INT
  printf '1\n' >"$TILO_SESSION_TEST_LOG.ignoring"
  while :; do sleep 1; done
fi
if [ "${TILO_SESSION_HOLD_PIPE:-0}" = 1 ]; then
  sleep 30 &
  printf '%s\n' "$!" >"$TILO_SESSION_TEST_LOG.holder"
  exit 9
fi
exit "${TILO_SESSION_DESKTOP_STATUS:-0}"
SH
cp "$fake_compositor" "$fake_native"
chmod 0755 "$fake_systemctl" "$fake_dbus_update" "$fake_native"
tilo_stage_script "$wrapper" "$test_wrapper" \
  SYSTEMCTL="$fake_systemctl" \
  DBUS_UPDATE="$fake_dbus_update" \
  COMPOSITOR_BIN="$fake_native" \
  IMAGE_FLAVOUR="$tmp/image-flavour" \
  FLIGHT_RECORDER="$TILO_FLIGHT_RECORDER" \
  RESTART_HOLD_S=2
printf 'dev\n' >"$tmp/image-flavour"
grep -Fqx "readonly SYSTEMCTL=$fake_systemctl" "$test_wrapper"
grep -Fqx "readonly COMPOSITOR_BIN=$fake_native" "$test_wrapper"

# A login with no flag runs the native session, under the lifecycle target and
# environment, with the remaining arguments and a flight record.
native_log="$tmp/native-log"
set +e
XDG_STATE_HOME="$tmp/native-state" XDG_RUNTIME_DIR="$runtime_dir" \
  TILO_SESSION_TEST_LOG="$native_log" TILO_SESSION_DESKTOP_STATUS=3 \
  "$test_wrapper" --backend drm >"$tmp/native-stdout"
status=$?
set -e
[ "$status" = 3 ]
cat >"$tmp/native-expected" <<'EOF'
systemctl --user unset-environment DISPLAY WAYLAND_DISPLAY XAUTHORITY DESKTOP_SESSION GDMSESSION XDG_SESSION_DESKTOP
dbus-update --systemd XDG_CURRENT_DESKTOP XDG_SESSION_TYPE GLIBC_TUNABLES
systemctl --user import-environment XDG_CURRENT_DESKTOP XDG_SESSION_TYPE GLIBC_TUNABLES
systemctl --user start tilo-graphical-session.target
native-session --backend drm
systemctl --user stop tilo-graphical-session.target
dbus-update --systemd WAYLAND_DISPLAY= DISPLAY= XAUTHORITY= XDG_CURRENT_DESKTOP= XDG_SESSION_DESKTOP= XDG_SESSION_TYPE= GLIBC_TUNABLES=
systemctl --user unset-environment DISPLAY WAYLAND_DISPLAY XAUTHORITY DESKTOP_SESSION GDMSESSION XDG_CURRENT_DESKTOP XDG_SESSION_DESKTOP XDG_SESSION_TYPE GLIBC_TUNABLES
EOF
cmp "$tmp/native-expected" "$native_log"
native_trace="$tmp/native-state/tilo/session-logs/frame-trace.csv"
grep -Fqx "native-session-output --backend drm trace=$native_trace" "$tmp/native-stdout"
native_flight="$tmp/native-state/tilo/session-logs/latest.log"
head -n 1 "$native_flight" | grep -Fq " shell=$fake_native"
grep -Fqx "native-session-output --backend drm trace=$native_trace" "$native_flight"
tail -n 1 "$native_flight" | grep -Eq '^tilo-flight-entry-v1 end=.* status=3 '
grep -Fqx 'handover=5' "$native_log.handover"
grep -Fqx 'handover-writable=yes' "$native_log.handover"
[ -z "$(ls -A "$runtime_dir")" ] || { ls -la "$runtime_dir" >&2; exit 1; }
# A missing native binary ends the login before any target starts.
missing_wrapper="$tmp/tilo-session-missing-native"
tilo_stage_script "$wrapper" "$missing_wrapper" \
  SYSTEMCTL="$fake_systemctl" \
  DBUS_UPDATE="$fake_dbus_update" \
  COMPOSITOR_BIN="$tmp/no-native-session" \
  IMAGE_FLAVOUR="$tmp/no-image-flavour" \
  FLIGHT_RECORDER="$TILO_FLIGHT_RECORDER"
set +e
XDG_STATE_HOME="$tmp/native-state" XDG_RUNTIME_DIR="$runtime_dir" \
  TILO_SESSION_TEST_LOG="$tmp/missing-native-log" \
  "$missing_wrapper" --backend drm 2>"$tmp/missing-native-err"
status=$?
set -e
[ "$status" = 1 ]
grep -Fq "$tmp/no-native-session is missing or not executable" "$tmp/missing-native-err"
[ ! -e "$tmp/missing-native-log" ]
[ -z "$(ls -A "$runtime_dir")" ] || { ls -la "$runtime_dir" >&2; exit 1; }

# An entry written before the flip still names the native session, which stays
# the default.
: >"$native_log"
rm -f "$native_log".control "$native_log".cpu-paths "$native_log".runs "$native_log".xdg
set +e
XDG_STATE_HOME="$tmp/native-state" XDG_RUNTIME_DIR="$runtime_dir" \
  TILO_SESSION_TEST_LOG="$native_log" TILO_SESSION_DESKTOP_STATUS=3 \
  "$test_wrapper" --native --backend drm >/dev/null
status=$?
set -e
[ "$status" = 3 ]
cmp "$tmp/native-expected" "$native_log"

# Successful verification starts the desktop, preserves its exit status, and
# tears down the Tilo target afterward.
export XDG_STATE_HOME="$tmp/state"
flight_dir="$XDG_STATE_HOME/tilo/session-logs"
set +e
XDG_RUNTIME_DIR="$runtime_dir" TILO_SESSION_TEST_LOG="$log" TILO_SESSION_DESKTOP_STATUS=7 TILO_FRAME_CPU_TRACE=/stale/untrusted \
  "$test_wrapper" --backend drm >"$tmp/stdout"
status=$?
set -e
[ "$status" = 7 ]
grep -Fqx disabled "$log.cpu-paths"
[ ! -e "$flight_dir/frame-cpu-path" ]
# The compositor's output still reaches the journal (the wrapper's stdout),
# and the flight record keeps it with a start and an end record.
grep -Fqx "native-session-output --backend drm trace=$flight_dir/frame-trace.csv" "$tmp/stdout"
head -n 1 "$flight_dir/latest.log" | grep -Eq '^tilo-flight-entry-v1 start=.* pid=[0-9]+ shell='
grep -Fqx "native-session-output --backend drm trace=$flight_dir/frame-trace.csv" "$flight_dir/latest.log"
tail -n 1 "$flight_dir/latest.log" | grep -Eq '^tilo-flight-entry-v1 end=.* status=7 logout_request=no restart_request=no$'
[ "$(stat -c %a "$flight_dir")" = 700 ]
# The log events, kept apart from the measurement stream, are bracketed by
# the same start and end records.
head -n 1 "$flight_dir/events.log" | grep -Eq '^tilo-flight-entry-v1 start=.* pid=[0-9]+ shell='
tail -n 1 "$flight_dir/events.log" | grep -Eq '^tilo-flight-entry-v1 end=.* status=7 logout_request=no restart_request=no$'
[ "$(stat -c %a "$flight_dir/events.log")" = 600 ]
# A dev image's compositor gets the control socket tilo-vm drives it through.
grep -Fqx "control=$runtime_dir/tilo-session/test-control.sock" "$log.control"
# It is still the person's login: a first login keeps identity setup.
grep -Fqx 'first-use=keep' "$log.control"
[ "$(stat -c %a "$flight_dir/latest.log")" = 600 ]
[ "$(stat -c %a "$flight_dir/frame-trace.csv")" = 600 ]
[ -z "$(ls -A "$runtime_dir")" ] || { ls -la "$runtime_dir" >&2; exit 1; }
# A recorder that starts slowly must still record a compositor that exits at
# once: the drain timeout covers only output already handed to the recorder.
slow_recorder="$tmp/slow-flight-recorder"
cat >"$slow_recorder" <<SH
#!/bin/bash
sleep 3
exec "$TILO_FLIGHT_RECORDER" "\$@"
SH
chmod 0755 "$slow_recorder"
slow_wrapper="$tmp/tilo-session-slow-recorder"
tilo_stage_script "$wrapper" "$slow_wrapper" \
  SYSTEMCTL="$fake_systemctl" \
  DBUS_UPDATE="$fake_dbus_update" \
  COMPOSITOR_BIN="$fake_native" \
  IMAGE_FLAVOUR="$tmp/no-image-flavour" \
  FLIGHT_RECORDER="$slow_recorder"
slow_dir="$tmp/slow-state/tilo/session-logs"
set +e
XDG_STATE_HOME="$tmp/slow-state" XDG_RUNTIME_DIR="$runtime_dir" \
  TILO_SESSION_TEST_LOG="$tmp/slow-log" TILO_SESSION_DESKTOP_STATUS=5 \
  timeout 20 "$slow_wrapper" --backend drm >"$tmp/slow-stdout"
status=$?
set -e
[ "$status" = 5 ]
# Other images set no control socket.
grep -Fqx 'control=' "$tmp/slow-log.control"
grep -Fqx 'first-use=' "$tmp/slow-log.control"
grep -Fqx "native-session-output --backend drm trace=$slow_dir/frame-trace.csv" "$tmp/slow-stdout"
head -n 1 "$slow_dir/latest.log" | grep -Eq '^tilo-flight-entry-v1 start=.* pid=[0-9]+ shell='
grep -Fqx "native-session-output --backend drm trace=$slow_dir/frame-trace.csv" "$slow_dir/latest.log"
tail -n 1 "$slow_dir/latest.log" | grep -Eq '^tilo-flight-entry-v1 end=.* status=5 logout_request=no restart_request=no$'
[ -z "$(ls -A "$runtime_dir")" ] || { ls -la "$runtime_dir" >&2; exit 1; }
# A login names no XDG base directories (neither PAM nor the user manager
# does); the compositor gets them under HOME, with the state directory its
# sealed collaboration state needs, owner-only.
bare_home="$tmp/bare-home"
install -d -m 700 "$bare_home"
set +e
env -u XDG_STATE_HOME -u XDG_DATA_HOME -u XDG_CONFIG_HOME HOME="$bare_home" \
  XDG_RUNTIME_DIR="$runtime_dir" TILO_SESSION_TEST_LOG="$tmp/bare-log" TILO_SESSION_DESKTOP_STATUS=5 \
  timeout 20 "$slow_wrapper" --backend drm >"$tmp/bare-stdout"
status=$?
set -e
[ "$status" = 5 ]
grep -Fqx "state=$bare_home/.local/state" "$tmp/bare-log.xdg"
grep -Fqx "data=$bare_home/.local/share" "$tmp/bare-log.xdg"
grep -Fqx "config=$bare_home/.config" "$tmp/bare-log.xdg"
[ "$(stat -c %a "$bare_home/.local/state")" = 700 ]
[ -s "$bare_home/.local/state/tilo/session-logs/latest.log" ]
# Directories the login already names are kept.
grep -Fqx "state=$tmp/slow-state" "$tmp/slow-log.xdg"
# The recorder persists its start record before it marks itself ready, so the
# wrapper may stop waiting on the record alone. A ready mark that comes after
# that must still be gone with the session, not left in the runtime directory.
late_ready_recorder="$tmp/late-ready-flight-recorder"
cat >"$late_ready_recorder" <<SH
#!/bin/bash
ready=\$TILO_FLIGHT_RECORD_READY
unset TILO_FLIGHT_RECORD_READY
(sleep 0.5; : >"\$ready") &
"$TILO_FLIGHT_RECORDER" "\$@"
status=\$?
wait
exit \$status
SH
chmod 0755 "$late_ready_recorder"
late_ready_wrapper="$tmp/tilo-session-late-ready"
tilo_stage_script "$wrapper" "$late_ready_wrapper" \
  SYSTEMCTL="$fake_systemctl" \
  DBUS_UPDATE="$fake_dbus_update" \
  COMPOSITOR_BIN="$fake_native" \
  IMAGE_FLAVOUR="$tmp/no-image-flavour" \
  FLIGHT_RECORDER="$late_ready_recorder"
set +e
XDG_STATE_HOME="$tmp/late-ready-state" XDG_RUNTIME_DIR="$runtime_dir" \
  TILO_SESSION_TEST_LOG="$tmp/late-ready-log" TILO_SESSION_DESKTOP_STATUS=5 \
  timeout 20 "$late_ready_wrapper" --backend drm >"$tmp/late-ready-stdout"
status=$?
set -e
[ "$status" = 5 ]
[ -z "$(ls -A "$runtime_dir")" ] || { ls -la "$runtime_dir" >&2; exit 1; }
# A full or failing disk must never keep the person from logging in. Here the
# state directory cannot exist at all (its parent is a file), so every write
# the wrapper and the recorder try fails, as on a full disk; the tests may run
# as a namespace root, which permissions would not stop. The desktop still
# starts, its exit status still comes back, and its output still reaches the
# journal (stdout).
mkdir -p "$tmp/full-state"
printf 'not a directory\n' >"$tmp/full-state/tilo"
full_dir="$tmp/full-state/tilo/session-logs"
set +e
XDG_STATE_HOME="$tmp/full-state" XDG_RUNTIME_DIR="$runtime_dir" \
  TILO_SESSION_TEST_LOG="$tmp/full-log" TILO_SESSION_DESKTOP_STATUS=6 \
  timeout 20 "$test_wrapper" --backend drm >"$tmp/full-stdout" 2>"$tmp/full-stderr"
status=$?
set -e
[ "$status" = 6 ] || { cat "$tmp/full-stderr" >&2; exit 1; }
grep -Fqx "native-session-output --backend drm trace=$full_dir/frame-trace.csv" "$tmp/full-stdout"
grep -Fqx 'native-session --backend drm' "$tmp/full-log"
[ -z "$(ls -A "$runtime_dir")" ] || { ls -la "$runtime_dir" >&2; exit 1; }
# A process that inherited the compositor's output must not keep the wrapper,
# and so the session unit, alive after the compositor exits. Its own state
# directory keeps this from rotating the record the checks below read.
held_log="$tmp/held-log"
set +e
XDG_STATE_HOME="$tmp/held-state" XDG_RUNTIME_DIR="$runtime_dir" \
  TILO_SESSION_TEST_LOG="$held_log" TILO_SESSION_HOLD_PIPE=1 \
  timeout 20 "$test_wrapper" --backend headless >/dev/null
held_status=$?
set -e
if [ -s "$held_log.holder" ]; then
  kill "$(cat "$held_log.holder")" 2>/dev/null || true
fi
[ "$held_status" = 9 ]
# Asked to finish, the recorder still publishes the output it relayed.
grep -Fqx "native-session-output --backend headless trace=$tmp/held-state/tilo/session-logs/frame-trace.csv" \
  "$tmp/held-state/tilo/session-logs/latest.log"
tail -n 1 "$tmp/held-state/tilo/session-logs/latest.log" \
  | grep -Eq '^tilo-flight-entry-v1 end=.* status=9 logout_request=no restart_request=no$'
cat >"$tmp/expected" <<'EOF'
systemctl --user unset-environment DISPLAY WAYLAND_DISPLAY XAUTHORITY DESKTOP_SESSION GDMSESSION XDG_SESSION_DESKTOP
dbus-update --systemd XDG_CURRENT_DESKTOP XDG_SESSION_TYPE GLIBC_TUNABLES
systemctl --user import-environment XDG_CURRENT_DESKTOP XDG_SESSION_TYPE GLIBC_TUNABLES
systemctl --user start tilo-graphical-session.target
native-session --backend drm
systemctl --user stop tilo-graphical-session.target
dbus-update --systemd WAYLAND_DISPLAY= DISPLAY= XAUTHORITY= XDG_CURRENT_DESKTOP= XDG_SESSION_DESKTOP= XDG_SESSION_TYPE= GLIBC_TUNABLES=
systemctl --user unset-environment DISPLAY WAYLAND_DISPLAY XAUTHORITY DESKTOP_SESSION GDMSESSION XDG_CURRENT_DESKTOP XDG_SESSION_DESKTOP XDG_SESSION_TYPE GLIBC_TUNABLES
EOF
cmp "$tmp/expected" "$log"

# Logout gets a private absolute channel from the wrapper, and the wrapper
# removes both the request file and its private directory after the compositor
# returns. This is the cross-process edge used by os-actions::Logout.
: >"$log"
XDG_RUNTIME_DIR="$runtime_dir" TILO_SESSION_TEST_LOG="$log" TILO_SESSION_WRITE_LOGOUT=1 \
  "$test_wrapper" --backend drm
cat >"$tmp/expected" <<'EOF'
systemctl --user unset-environment DISPLAY WAYLAND_DISPLAY XAUTHORITY DESKTOP_SESSION GDMSESSION XDG_SESSION_DESKTOP
dbus-update --systemd XDG_CURRENT_DESKTOP XDG_SESSION_TYPE GLIBC_TUNABLES
systemctl --user import-environment XDG_CURRENT_DESKTOP XDG_SESSION_TYPE GLIBC_TUNABLES
systemctl --user start tilo-graphical-session.target
native-session --backend drm
systemctl --user stop tilo-graphical-session.target
dbus-update --systemd WAYLAND_DISPLAY= DISPLAY= XAUTHORITY= XDG_CURRENT_DESKTOP= XDG_SESSION_DESKTOP= XDG_SESSION_TYPE= GLIBC_TUNABLES=
systemctl --user unset-environment DISPLAY WAYLAND_DISPLAY XAUTHORITY DESKTOP_SESSION GDMSESSION XDG_CURRENT_DESKTOP XDG_SESSION_DESKTOP XDG_SESSION_TYPE GLIBC_TUNABLES
EOF
cmp "$tmp/expected" "$log"
# The next login keeps the previous record, which tilo-session-report reads.
grep -Fq 'status=7' "$flight_dir/previous.log"
grep -Fq 'status=7' "$flight_dir/previous-events.log"
tail -n 1 "$flight_dir/events.log" | grep -Eq 'status=0 logout_request=yes restart_request=no$'
tail -n 1 "$flight_dir/latest.log" | grep -Eq 'status=0 logout_request=yes restart_request=no$'
"$TILO_SESSION_REPORT" --root "$flight_dir" --previous >"$tmp/report"
python3 - "$tmp/report" <<'PY'
import json, sys
report = json.load(open(sys.argv[1]))
assert report["schema"] == "tilo-session-report-v1", report
assert report["flight"]["available"] is True, report
assert report["frames"]["available"] is True, report
PY
request_path=$(cat "$log.request-path")
case "$request_path" in
  /*) ;;
  *)
    echo 'logout request path is not absolute' >&2
    exit 1
    ;;
esac
[ ! -e "$request_path" ]
[ ! -d "$(dirname "$request_path")" ]

# A missing identity reaches the native compositor, which owns the interactive
# first-login OOBE. The wrapper must not preflight an identity that only that
# compositor can create.
: >"$log"
XDG_RUNTIME_DIR="$runtime_dir" TILO_SESSION_TEST_LOG="$log" TILO_SESSION_VERIFY_SEQUENCE=1 \
  "$test_wrapper" --backend drm
cat >"$tmp/expected" <<'EOF'
systemctl --user unset-environment DISPLAY WAYLAND_DISPLAY XAUTHORITY DESKTOP_SESSION GDMSESSION XDG_SESSION_DESKTOP
dbus-update --systemd XDG_CURRENT_DESKTOP XDG_SESSION_TYPE GLIBC_TUNABLES
systemctl --user import-environment XDG_CURRENT_DESKTOP XDG_SESSION_TYPE GLIBC_TUNABLES
systemctl --user start tilo-graphical-session.target
native-session --backend drm
systemctl --user stop tilo-graphical-session.target
dbus-update --systemd WAYLAND_DISPLAY= DISPLAY= XAUTHORITY= XDG_CURRENT_DESKTOP= XDG_SESSION_DESKTOP= XDG_SESSION_TYPE= GLIBC_TUNABLES=
systemctl --user unset-environment DISPLAY WAYLAND_DISPLAY XAUTHORITY DESKTOP_SESSION GDMSESSION XDG_CURRENT_DESKTOP XDG_SESSION_DESKTOP XDG_SESSION_TYPE GLIBC_TUNABLES
EOF
cmp "$tmp/expected" "$log"

# Session termination is forwarded to the compositor and cleanup still stops
# the lifecycle target.
: >"$log"
rm -f "$log.verify-count" "$log.holding"
XDG_RUNTIME_DIR="$runtime_dir" TILO_SESSION_TEST_LOG="$log" TILO_SESSION_HOLD=1 \
  "$test_wrapper" --backend drm &
session_pid=$!
for _ in $(seq 1 500); do
  [ -s "$log.holding" ] && break
  sleep 0.02
done
# The desktop log precedes the TERM handler; the holding marker follows it.
[ -s "$log.holding" ]
grep -Fq 'native-session --backend drm' "$log"
kill -TERM "$session_pid"
wait "$session_pid" || [ "$?" = 143 ]
grep -Fqx 'native-session-term' "$log"
[ "$(grep -Fc 'systemctl --user stop tilo-graphical-session.target' "$log")" = 1 ]

# A compositor that cannot be stopped must not hold the login, and with it this
# device's workspace service: the service's invocation journal admits one writer, so
# a login that will not end keeps the next login from opening it. The wrapper ends
# the compositor once its bound is spent, so the login ends and its private runtime
# directory goes with it. The bound is staged short here; the shipped one is longer
# than the compositor's own watchdog and ordered teardown.
: >"$log"
rm -f "$log.ignoring"
short_stop_wrapper="$tmp/tilo-session-short-stop"
tilo_stage_script "$wrapper" "$short_stop_wrapper" \
  SYSTEMCTL="$fake_systemctl" \
  DBUS_UPDATE="$fake_dbus_update" \
  COMPOSITOR_BIN="$fake_native" \
  IMAGE_FLAVOUR="$tmp/no-image-flavour" \
  FLIGHT_RECORDER="$TILO_FLIGHT_RECORDER" \
  EXIT_WAIT_TENTHS=10
XDG_RUNTIME_DIR="$runtime_dir" TILO_SESSION_TEST_LOG="$log" TILO_SESSION_IGNORE_TERM=1 \
  "$short_stop_wrapper" --backend drm >"$tmp/short-stop-stdout" 2>"$tmp/short-stop-stderr" &
session_pid=$!
for _ in $(seq 1 500); do
  [ -s "$log.ignoring" ] && break
  sleep 0.02
done
[ -s "$log.ignoring" ] || { echo 'the ignoring compositor never started' >&2; exit 1; }
stopped_at=$(date +%s)
kill -TERM "$session_pid"
set +e
wait "$session_pid"
status=$?
set -e
[ "$status" = 143 ] || { echo "a login with an unkillable compositor ended with $status" >&2; exit 1; }
# The wrapper does not wait for a compositor that will not stop.
[ "$(( $(date +%s) - stopped_at ))" -lt 10 ] || {
  echo 'the wrapper waited for a compositor that ignored every stop signal' >&2
  exit 1
}
# The wrapper's own word goes where the desktop's output goes — the login's saved
# output and its stdout — because it says so while that output is redirected: it
# says the compositor was ended.
grep -Fq 'the desktop did not stop in 1 s; ending it' "$tmp/short-stop-stdout" || {
  echo 'the wrapper did not say it ended a compositor that ignored the stop' >&2
  cat "$tmp/short-stop-stdout" >&2
  cat "$tmp/short-stop-stderr" >&2
  exit 1
}
[ -z "$(ls -A "$runtime_dir")" ] || { ls -la "$runtime_dir" >&2; exit 1; }

# A desktop restart (SIGUSR1) stops the compositor and starts it again in the
# same login: the target is started once and stopped once, at the end. The
# restarted run's record becomes previous.log. The second compositor exiting
# by itself ends the session once nobody asked again within the wrapper's
# wait: only a requested restart starts another.
: >"$log"
rm -f "$log.runs" "$log.holding" "$log.cpu-paths" "$log.markers"
restart_state="$tmp/restart-state"
rm -f "$log.recovery"
XDG_STATE_HOME="$restart_state" XDG_RUNTIME_DIR="$runtime_dir" TILO_SESSION_TEST_LOG="$log" \
  TILO_SESSION_HOLD_FIRST=1 TILO_SESSION_DESKTOP_STATUS=7 TILO_VM_FRAME_CPU_TRACE=1 \
  "$test_wrapper" --backend drm >/dev/null &
session_pid=$!
# The signal goes to the wrapper alone (timeout(1) would pass it on to the
# whole process group), once the compositor handles TERM as the real one does.
for _ in $(seq 1 500); do
  [ -s "$log.holding" ] && break
  sleep 0.02
done
[ -s "$log.holding" ]
kill -USR1 "$session_pid"
set +e
wait "$session_pid"
status=$?
set -e
[ "$status" = 7 ] || { echo "restarted session ended with $status" >&2; exit 1; }
[ "$(cat "$log.runs")" = 2 ]
# The restart was announced to both runs: the run that ended found no marker of its own, the run
# that followed read the one the wrapper wrote and consumed it.
printf 'restart-marker=no\nrestart-marker=yes\n' >"$tmp/expected-markers"
cmp "$tmp/expected-markers" "$log.markers"
printf 'unlocked\nlocked\n' >"$tmp/expected-recovery"
cmp "$tmp/expected-recovery" "$log.recovery"
cat >"$tmp/expected" <<'EOF'
systemctl --user unset-environment DISPLAY WAYLAND_DISPLAY XAUTHORITY DESKTOP_SESSION GDMSESSION XDG_SESSION_DESKTOP
dbus-update --systemd XDG_CURRENT_DESKTOP XDG_SESSION_TYPE GLIBC_TUNABLES
systemctl --user import-environment XDG_CURRENT_DESKTOP XDG_SESSION_TYPE GLIBC_TUNABLES
systemctl --user start tilo-graphical-session.target
native-session --backend drm
native-session-term
native-session --backend drm
systemctl --user stop tilo-graphical-session.target
dbus-update --systemd WAYLAND_DISPLAY= DISPLAY= XAUTHORITY= XDG_CURRENT_DESKTOP= XDG_SESSION_DESKTOP= XDG_SESSION_TYPE= GLIBC_TUNABLES=
systemctl --user unset-environment DISPLAY WAYLAND_DISPLAY XAUTHORITY DESKTOP_SESSION GDMSESSION XDG_CURRENT_DESKTOP XDG_SESSION_DESKTOP XDG_SESSION_TYPE GLIBC_TUNABLES
EOF
cmp "$tmp/expected" "$log"
restart_dir="$restart_state/tilo/session-logs"
[ "$(wc -l <"$log.cpu-paths")" = 2 ]
[ "$(sort -u "$log.cpu-paths" | wc -l)" = 2 ]
[ "$(cat "$restart_dir/frame-cpu-path")" = "$(tail -n 1 "$log.cpu-paths")" ]
[ "$(stat -c %a "$restart_dir/frame-cpu-path")" = 600 ]
while IFS= read -r cpu_path; do
  [ -f "$cpu_path" ]
  [ "$(stat -c %a "$cpu_path")" = 600 ]
  [ "$(stat -c %a "$(dirname "$cpu_path")")" = 700 ]
done <"$log.cpu-paths"

tail -n 1 "$restart_dir/previous.log" \
  | grep -Eq '^tilo-flight-entry-v1 end=.* status=0 logout_request=no restart_request=yes$'
head -n 1 "$restart_dir/latest.log" | grep -Eq '^tilo-flight-entry-v1 start=.* pid=[0-9]+ shell='
tail -n 1 "$restart_dir/latest.log" \
  | grep -Eq '^tilo-flight-entry-v1 end=.* status=7 logout_request=no restart_request=no$'
[ -z "$(ls -A "$runtime_dir")" ] || { ls -la "$runtime_dir" >&2; exit 1; }

# A logout asked for while the restart is under way still wins: no second compositor starts, and
# the marker the restart wrote leaves nothing behind.
: >"$log"
rm -f "$log.runs" "$log.markers" "$log.holding"
XDG_RUNTIME_DIR="$runtime_dir" TILO_SESSION_TEST_LOG="$log" TILO_SESSION_HOLD=1 TILO_SESSION_WRITE_LOGOUT=1 \
  "$test_wrapper" --backend drm >/dev/null &
session_pid=$!
for _ in $(seq 1 500); do
  [ -s "$log.holding" ] && break
  sleep 0.02
done
[ -s "$log.holding" ]
kill -USR1 "$session_pid"
set +e
wait "$session_pid"
status=$?
set -e
[ "$status" = 0 ] || { echo "a logged-out restart ended with $status" >&2; exit 1; }
[ "$(cat "$log.runs")" = 1 ] || { echo 'a logout did not win over the restart' >&2; exit 1; }
grep -Fxq 'restart-marker=no' "$log.markers"
[ -z "$(ls -A "$runtime_dir")" ] || { ls -la "$runtime_dir" >&2; exit 1; }

# A restart whose new desktop never starts leaves no marker for a later read: the marker is in
# the login's private directory, which goes with the login.
: >"$log"
rm -f "$log.runs" "$log.markers" "$log.holding" "$log.request-path"
XDG_RUNTIME_DIR="$runtime_dir" TILO_SESSION_TEST_LOG="$log" TILO_SESSION_HOLD_FIRST=1 \
  TILO_SESSION_TERM_DELAY=3 "$test_wrapper" --backend drm >/dev/null &
session_pid=$!
for _ in $(seq 1 500); do
  [ -s "$log.holding" ] && break
  sleep 0.02
done
[ -s "$log.holding" ]
channel="$(dirname "$(cat "$log.request-path")")"
kill -USR1 "$session_pid"
for _ in $(seq 1 500); do
  [ -e "$channel/restart-request" ] && break
  sleep 0.02
done
[ -e "$channel/restart-request" ] || { echo 'the restart was not announced' >&2; exit 1; }
# The compositor is still stopping, so the login ends before another one could start.
kill -TERM "$session_pid"
set +e
wait "$session_pid"
status=$?
set -e
[ "$status" = 143 ] || { echo "a cancelled restart ended with $status" >&2; exit 1; }
[ "$(cat "$log.runs")" = 1 ] || { echo 'a compositor started after the restart was cancelled' >&2; exit 1; }
[ ! -d "$channel" ] || { ls -la "$channel" >&2; exit 1; }
[ -z "$(ls -A "$runtime_dir")" ] || { ls -la "$runtime_dir" >&2; exit 1; }

# The desktop an asked restart started never becomes usable (an update's new desktop program
# that does not start): the login stays, says so beside the desktop's records, and the restart
# asked again starts the compositor once more. Nobody loses their apps to a failed replacement.
: >"$log"
rm -f "$log.runs" "$log.markers" "$log.holding" "$log.request-path"
install -d -m 700 "$runtime_dir/tilo-session"
XDG_RUNTIME_DIR="$runtime_dir" TILO_SESSION_TEST_LOG="$log" TILO_SESSION_HOLD_FIRST=1 \
  TILO_SESSION_DESKTOP_STATUS=7 "$test_wrapper" --backend drm >/dev/null &
session_pid=$!
for _ in $(seq 1 500); do
  [ -s "$log.holding" ] && break
  sleep 0.02
done
[ -s "$log.holding" ]
kill -USR1 "$session_pid"
failed="$runtime_dir/tilo-session/desktop-restart-failed"
for _ in $(seq 1 500); do
  [ -e "$failed" ] && break
  sleep 0.02
done
[ -e "$failed" ] || { echo 'a restart whose desktop never started was not said' >&2; exit 1; }
kill -0 "$session_pid" || { echo 'the login ended over a restart that failed' >&2; exit 1; }
[ "$(cat "$log.runs")" = 2 ]
kill -USR1 "$session_pid"
for _ in $(seq 1 500); do
  [ "$(cat "$log.runs")" = 3 ] && break
  sleep 0.02
done
[ "$(cat "$log.runs")" = 3 ] || { echo 'the restart asked again started no compositor' >&2; exit 1; }
# The third run fails as well and nobody asks again: the login ends with its status.
set +e
wait "$session_pid"
status=$?
set -e
[ "$status" = 7 ] || { echo "a login nobody asked to restart again ended with $status" >&2; exit 1; }
[ "$(cat "$log.runs")" = 3 ]
[ ! -e "$failed" ] || { echo 'the failed restart is still said after the login ended' >&2; exit 1; }

# A restarted desktop that was usable and then ends is the compositor's own end: the login ends
# at once, as it does for the first run, and never holds itself for the restart to be asked again.
: >"$log"
rm -f "$log.runs" "$log.markers" "$log.holding" "$log.request-path" "$log.out" \
  "$runtime_dir/tilo-session/desktop-ready"
XDG_RUNTIME_DIR="$runtime_dir" TILO_SESSION_TEST_LOG="$log" TILO_SESSION_HOLD_FIRST=1 \
  TILO_SESSION_PUBLISH_READY=1 TILO_SESSION_DESKTOP_STATUS=7 "$test_wrapper" --backend drm >"$log.out" &
session_pid=$!
for _ in $(seq 1 500); do
  [ -s "$log.holding" ] && break
  sleep 0.02
done
[ -s "$log.holding" ]
kill -USR1 "$session_pid"
set +e
wait "$session_pid"
status=$?
set -e
[ "$status" = 7 ] || { echo "a usable restarted desktop ended the login with $status" >&2; exit 1; }
[ "$(cat "$log.runs")" = 2 ]
! grep -q 'the login stays .* s for the restart to be asked again' "$log.out" \
  || { echo 'a usable desktop that ended kept the login waiting' >&2; exit 1; }
rm -f "$log.out"
rm -rf "$runtime_dir/tilo-session"
[ -z "$(ls -A "$runtime_dir")" ] || { ls -la "$runtime_dir" >&2; exit 1; }

printf 'Tilo session lifecycle contract: PASS\n'
