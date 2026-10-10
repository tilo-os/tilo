#!/usr/bin/env bash
# Source/runtime contract for the greetd -> systemd user target -> verified
# identity -> compositor lifecycle. Uses a rewritten temporary copy so the
# production wrapper keeps absolute, non-overridable trusted command paths.
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
. "$repo/tools/desktop-isolation.sh"
tilo_test_isolate "$@"
python3 "$repo/os/tests/flight_recorder_test.py"
unset GNOME_KEYRING_CONTROL
wrapper="$repo/os/usr/libexec/tilo-session"
target="$repo/os/usr/lib/systemd/user/tilo-graphical-session.target"
selinux_policy="$repo/os/usr/share/tilo/selinux/tilo-base.cil"
identity_unit="$repo/os/usr/lib/systemd/user/tilo-identity.service"
recipient_unit="$repo/os/usr/lib/systemd/user/tilo-fs-recipient.service"
recipient_socket="$repo/os/usr/lib/systemd/user/tilo-fs-recipient.socket"
release_workflow="$repo/.forgejo/workflows/build-image.yml"
shell_main="$repo/crates/session/shell/src/linux_main.rs"

grep -Fqx 'readonly DBUS_UPDATE=/usr/bin/dbus-update-activation-environment' "$wrapper"
grep -Fqx '"$DBUS_UPDATE" --systemd XDG_CURRENT_DESKTOP XDG_SESSION_TYPE' "$wrapper"
grep -Fqx '"$SYSTEMCTL" --user import-environment XDG_CURRENT_DESKTOP XDG_SESSION_TYPE' "$wrapper"
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
if [ -e "$identity_unit" ] || grep -q 'tilo-identity\.service' "$recipient_unit"; then
  echo 'a pre-session identity unit is back' >&2
  exit 1
fi
# Helper entry points must return before long-lived compositor services bind.
# GPU selection intentionally happens earlier now so every process shape uses
# the same explicit device policy; line-order checks against an older startup
# layout made this lifecycle test fail without proving a runtime defect.
identity_prepare_line=$(grep -n 'if let Some(exit_code) = prepare_identity(provision_identity_requested)' "$shell_main" | cut -d: -f1)
invite_broker_line=$(grep -n 'let _invite_broker = match identity_provisioning::start_configured_invite_broker()' "$shell_main" | cut -d: -f1)
if [ -z "$identity_prepare_line" ] || [ -z "$invite_broker_line" ] \
   || [ "$identity_prepare_line" -ge "$invite_broker_line" ]; then
  echo 'identity helper no longer returns before compositor services start' >&2
  exit 1
fi
grep -Fqx 'PartOf=graphical-session.target' "$recipient_unit"
grep -Fqx 'PartOf=graphical-session.target' "$recipient_socket"
grep -Fqx 'WantedBy=graphical-session.target' "$recipient_socket"
grep -Fqx 'ListenStream=%t/tilo/services/tilo-fs.sock' "$recipient_socket"
grep -Fqx 'Service=tilo-fs-recipient.service' "$recipient_socket"
for policy in \
  'ProtectSystem=strict' \
  'ProtectHome=read-only' \
  'ReadWritePaths=%h/Shared %h/.cache/tilo %h/.cache/tilo/received-files %t/tilo' \
  'PrivateIPC=yes' \
  'ProtectKernelLogs=yes' \
  'ProtectClock=yes' \
  'ProtectHostname=yes' \
  'RestrictRealtime=yes' \
  'KeyringMode=private' \
  'RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6'; do
  grep -Fqx "$policy" "$recipient_unit"
done
# The production sandbox holds everywhere, dev images included: a tilo-vm
# slot's user manager runs these units exactly as a login does.
for unit in tilo-fs-recipient.service tilo-fs-cloud.service tilo-models.service; do
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
grep -Fqx 'Exec=/usr/libexec/tilo-session --backend drm' \
  "$repo/os/usr/share/wayland-sessions/shell.desktop"
[ "$(ls "$repo/os/usr/share/wayland-sessions")" = shell.desktop ] || {
  echo 'the image must ship exactly one login session: shell.desktop' >&2
  exit 1
}
grep -Fq 'missing="$(ldd /usr/bin/shell | awk '\''/not found/ { print $1 }'\'')"' \
  "$repo/os/Containerfile"
grep -Fq 'if [ -n "$missing" ]; then' "$repo/os/Containerfile"
grep -Fq 'shell has unresolved runtime libraries:' "$repo/os/Containerfile"
engine_deps_line=$(grep -n '&& gst-inspect-1.0 aacparse >/dev/null' "$repo/os/Containerfile" | tail -1 | cut -d: -f1)
shell_link_check_line=$(grep -n 'missing="$(ldd /usr/bin/shell' "$repo/os/Containerfile" | tail -1 | cut -d: -f1)
test "$engine_deps_line" -lt "$shell_link_check_line"
grep -Fq 'the browser engine has unresolved runtime libraries:' "$repo/os/Containerfile"
grep -Fq 'busctl --user status org.freedesktop.impl.portal.desktop.tilo' "$release_workflow"
# First-use actions are now driven through the installed AT-SPI endpoint;
# keeping stale blind-keyboard assertions here made the FUSE lane reject the
# stronger accessibility release gate before any filesystem test could run.
grep -Fq './os/dev/vm.sh ssh /usr/libexec/tilo/native-atspi-smoke.sh oobe-actions' "$release_workflow"
grep -Fq './os/dev/vm.sh keys tab ret' "$release_workflow"
grep -Fq 'identity ceremony did not produce the certified chain' "$release_workflow"

tmp_root="${TMPDIR:-/tmp}"
tmp=$(mktemp -d "$tmp_root/tilo-session-lifecycle.XXXXXX")
cleanup() {
  case "$tmp" in "$tmp_root"/tilo-session-lifecycle.*) rm -r "$tmp" ;; esac
}
trap cleanup EXIT INT TERM

log="$tmp/log"
fake_systemctl="$tmp/systemctl"
fake_dbus_update="$tmp/dbus-update-activation-environment"
fake_shell="$tmp/shell"
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
cat >"$fake_shell" <<'SH'
#!/bin/bash
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
printf 'desktop %s\n' "$*" >>"$TILO_SESSION_TEST_LOG"
runs_file="$TILO_SESSION_TEST_LOG.runs"
runs=0
[ ! -f "$runs_file" ] || runs=$(cat "$runs_file")
runs=$((runs + 1))
printf '%s\n' "$runs" >"$runs_file"
# Observe the real wrapper's recovery channel before simulating a compositor
# that hands its locked state back on ordered teardown.
recovery_file="$(dirname "$TILO_SESSION_LOGOUT_REQUEST")/lock-state"
cat "$recovery_file" >>"$TILO_SESSION_TEST_LOG.recovery"
[ "$runs" != 1 ] || printf 'locked\n' >"$recovery_file"
printf 'control=%s\nfirst-use=%s\n' "${TILO_TEST_CONTROL:-}" "${TILO_TEST_CONTROL_FIRST_USE:-}" \
  >"$TILO_SESSION_TEST_LOG.control"
printf 'desktop-output %s trace=%s\n' "$*" "${TILO_FRAME_TRACE:-}"
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
if [ "${TILO_SESSION_HOLD:-0}" = 1 ] \
   || { [ "${TILO_SESSION_HOLD_FIRST:-0}" = 1 ] && [ "$runs" = 1 ]; }; then
  trap 'printf "desktop-term\n" >>"$TILO_SESSION_TEST_LOG"; exit 0' TERM HUP INT
  printf '%s\n' "$runs" >"$TILO_SESSION_TEST_LOG.holding"
  while :; do sleep 1; done
fi
if [ "${TILO_SESSION_HOLD_PIPE:-0}" = 1 ]; then
  sleep 30 &
  printf '%s\n' "$!" >"$TILO_SESSION_TEST_LOG.holder"
  exit 9
fi
exit "${TILO_SESSION_DESKTOP_STATUS:-0}"
SH
chmod 0755 "$fake_systemctl" "$fake_dbus_update" "$fake_shell"
tilo_stage_script "$wrapper" "$test_wrapper" \
  SYSTEMCTL="$fake_systemctl" \
  DBUS_UPDATE="$fake_dbus_update" \
  SHELL_BIN="$fake_shell" \
  IMAGE_FLAVOUR="$tmp/image-flavour" \
  FLIGHT_RECORDER="$repo/dist/session/tilo-flight-recorder.py"
printf 'dev\n' >"$tmp/image-flavour"
grep -Fqx "readonly SYSTEMCTL=$fake_systemctl" "$test_wrapper"
grep -Fqx "readonly SHELL_BIN=$fake_shell" "$test_wrapper"

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
grep -Fqx "desktop-output --backend drm trace=$flight_dir/frame-trace.csv" "$tmp/stdout"
head -n 1 "$flight_dir/latest.log" | grep -Eq '^tilo-flight-record-v1 start=.* pid=[0-9]+ shell='
grep -Fqx "desktop-output --backend drm trace=$flight_dir/frame-trace.csv" "$flight_dir/latest.log"
tail -n 1 "$flight_dir/latest.log" | grep -Eq '^tilo-flight-record-v1 end=.* status=7 logout_request=no restart_request=no$'
[ "$(stat -c %a "$flight_dir")" = 700 ]
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
exec python3 "$repo/dist/session/tilo-flight-recorder.py" "\$@"
SH
chmod 0755 "$slow_recorder"
slow_wrapper="$tmp/tilo-session-slow-recorder"
tilo_stage_script "$wrapper" "$slow_wrapper" \
  SYSTEMCTL="$fake_systemctl" \
  DBUS_UPDATE="$fake_dbus_update" \
  SHELL_BIN="$fake_shell" \
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
grep -Fqx "desktop-output --backend drm trace=$slow_dir/frame-trace.csv" "$tmp/slow-stdout"
head -n 1 "$slow_dir/latest.log" | grep -Eq '^tilo-flight-record-v1 start=.* pid=[0-9]+ shell='
grep -Fqx "desktop-output --backend drm trace=$slow_dir/frame-trace.csv" "$slow_dir/latest.log"
tail -n 1 "$slow_dir/latest.log" | grep -Eq '^tilo-flight-record-v1 end=.* status=5 logout_request=no restart_request=no$'
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
tail -n 1 "$tmp/held-state/tilo/session-logs/latest.log" \
  | grep -Eq '^tilo-flight-record-v1 end=.* status=9 logout_request=no restart_request=no$'
cat >"$tmp/expected" <<'EOF'
systemctl --user unset-environment DISPLAY WAYLAND_DISPLAY XAUTHORITY DESKTOP_SESSION GDMSESSION XDG_SESSION_DESKTOP
dbus-update --systemd XDG_CURRENT_DESKTOP XDG_SESSION_TYPE
systemctl --user import-environment XDG_CURRENT_DESKTOP XDG_SESSION_TYPE
systemctl --user start tilo-graphical-session.target
desktop --backend drm
systemctl --user stop tilo-graphical-session.target
dbus-update --systemd WAYLAND_DISPLAY= DISPLAY= XAUTHORITY= XDG_CURRENT_DESKTOP= XDG_SESSION_DESKTOP= XDG_SESSION_TYPE=
systemctl --user unset-environment DISPLAY WAYLAND_DISPLAY XAUTHORITY DESKTOP_SESSION GDMSESSION XDG_CURRENT_DESKTOP XDG_SESSION_DESKTOP XDG_SESSION_TYPE
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
dbus-update --systemd XDG_CURRENT_DESKTOP XDG_SESSION_TYPE
systemctl --user import-environment XDG_CURRENT_DESKTOP XDG_SESSION_TYPE
systemctl --user start tilo-graphical-session.target
desktop --backend drm
systemctl --user stop tilo-graphical-session.target
dbus-update --systemd WAYLAND_DISPLAY= DISPLAY= XAUTHORITY= XDG_CURRENT_DESKTOP= XDG_SESSION_DESKTOP= XDG_SESSION_TYPE=
systemctl --user unset-environment DISPLAY WAYLAND_DISPLAY XAUTHORITY DESKTOP_SESSION GDMSESSION XDG_CURRENT_DESKTOP XDG_SESSION_DESKTOP XDG_SESSION_TYPE
EOF
cmp "$tmp/expected" "$log"
# The next login keeps the previous record, which tilo-session-report reads.
grep -Fq 'status=7' "$flight_dir/previous.log"
tail -n 1 "$flight_dir/latest.log" | grep -Eq 'status=0 logout_request=yes restart_request=no$'
python3 "$repo/dist/session/tilo-session-report.py" --root "$flight_dir" --previous >"$tmp/report"
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
dbus-update --systemd XDG_CURRENT_DESKTOP XDG_SESSION_TYPE
systemctl --user import-environment XDG_CURRENT_DESKTOP XDG_SESSION_TYPE
systemctl --user start tilo-graphical-session.target
desktop --backend drm
systemctl --user stop tilo-graphical-session.target
dbus-update --systemd WAYLAND_DISPLAY= DISPLAY= XAUTHORITY= XDG_CURRENT_DESKTOP= XDG_SESSION_DESKTOP= XDG_SESSION_TYPE=
systemctl --user unset-environment DISPLAY WAYLAND_DISPLAY XAUTHORITY DESKTOP_SESSION GDMSESSION XDG_CURRENT_DESKTOP XDG_SESSION_DESKTOP XDG_SESSION_TYPE
EOF
cmp "$tmp/expected" "$log"

# Session termination is forwarded to the compositor and cleanup still stops
# the lifecycle target.
: >"$log"
rm -f "$log.verify-count" "$log.holding"
XDG_RUNTIME_DIR="$runtime_dir" TILO_SESSION_TEST_LOG="$log" TILO_SESSION_HOLD=1 \
  "$test_wrapper" --backend drm &
session_pid=$!
for _ in $(seq 1 100); do
  [ -s "$log.holding" ] && break
  sleep 0.02
done
# The desktop log precedes the TERM handler; the holding marker follows it.
[ -s "$log.holding" ]
grep -Fq 'desktop --backend drm' "$log"
kill -TERM "$session_pid"
wait "$session_pid" || [ "$?" = 143 ]
grep -Fqx 'desktop-term' "$log"
[ "$(grep -Fc 'systemctl --user stop tilo-graphical-session.target' "$log")" = 1 ]

# A desktop restart (SIGUSR1) stops the compositor and starts it again in the
# same login: the target is started once and stopped once, at the end. The
# restarted run's record becomes previous.log. The second compositor exiting
# by itself ends the session: only a requested restart starts another.
: >"$log"
rm -f "$log.runs" "$log.holding" "$log.cpu-paths"
restart_state="$tmp/restart-state"
rm -f "$log.recovery"
XDG_STATE_HOME="$restart_state" XDG_RUNTIME_DIR="$runtime_dir" TILO_SESSION_TEST_LOG="$log" \
  TILO_SESSION_HOLD_FIRST=1 TILO_SESSION_DESKTOP_STATUS=7 TILO_VM_FRAME_CPU_TRACE=1 \
  "$test_wrapper" --backend drm >/dev/null &
session_pid=$!
# The signal goes to the wrapper alone (timeout(1) would pass it on to the
# whole process group), once the compositor handles TERM as the real one does.
for _ in $(seq 1 250); do
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
printf 'unlocked\nlocked\n' >"$tmp/expected-recovery"
cmp "$tmp/expected-recovery" "$log.recovery"
cat >"$tmp/expected" <<'EOF'
systemctl --user unset-environment DISPLAY WAYLAND_DISPLAY XAUTHORITY DESKTOP_SESSION GDMSESSION XDG_SESSION_DESKTOP
dbus-update --systemd XDG_CURRENT_DESKTOP XDG_SESSION_TYPE
systemctl --user import-environment XDG_CURRENT_DESKTOP XDG_SESSION_TYPE
systemctl --user start tilo-graphical-session.target
desktop --backend drm
desktop-term
desktop --backend drm
systemctl --user stop tilo-graphical-session.target
dbus-update --systemd WAYLAND_DISPLAY= DISPLAY= XAUTHORITY= XDG_CURRENT_DESKTOP= XDG_SESSION_DESKTOP= XDG_SESSION_TYPE=
systemctl --user unset-environment DISPLAY WAYLAND_DISPLAY XAUTHORITY DESKTOP_SESSION GDMSESSION XDG_CURRENT_DESKTOP XDG_SESSION_DESKTOP XDG_SESSION_TYPE
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
  | grep -Eq '^tilo-flight-record-v1 end=.* status=0 logout_request=no restart_request=yes$'
head -n 1 "$restart_dir/latest.log" | grep -Eq '^tilo-flight-record-v1 start=.* pid=[0-9]+ shell='
tail -n 1 "$restart_dir/latest.log" \
  | grep -Eq '^tilo-flight-record-v1 end=.* status=7 logout_request=no restart_request=no$'
[ -z "$(ls -A "$runtime_dir")" ] || { ls -la "$runtime_dir" >&2; exit 1; }

printf 'Tilo session lifecycle contract: PASS\n'
