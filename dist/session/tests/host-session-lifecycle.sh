#!/usr/bin/env bash
# Exercise the packaged display-manager wrapper without touching the caller's
# real user manager or graphical session.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
. "$repo_root/tools/desktop-isolation.sh"
tilo_test_isolate "$@"
work=$(mktemp -d)
cleanup() {
    if [ "${TILO_SESSION_TEST_KEEP:-0}" = 1 ]; then
        printf 'kept host-session fixture at %s\n' "$work" >&2
    else
        rm -rf -- "$work"
    fi
}
trap cleanup EXIT

log=$work/log
cat >"$work/systemctl" <<'SH'
#!/bin/sh
printf 'systemctl %s\n' "$*" >>"$TILO_SESSION_TEST_LOG"
if [ "${TILO_SESSION_TEST_FAIL_IMPORT:-0}" = 1 ] && [ "${2:-}" = import-environment ]; then
    exit 1
fi
if [ "${TILO_SESSION_TEST_FAIL_START:-0}" = 1 ] && [ "${2:-}" = start ]; then
    exit 1
fi
if [ "${2:-}" = is-active ]; then
    if [ "${TILO_SESSION_TEST_PREDECESSOR:-0}" = 1 ] \
        && [ "${4:-}" = graphical-session.target ]; then
        exit 0
    fi
    exit 3
fi
exit 0
SH
cat >"$work/dbus-update-activation-environment" <<'SH'
#!/bin/sh
printf 'dbus-update %s\n' "$*" >>"$TILO_SESSION_TEST_LOG"
if [ "${TILO_SESSION_TEST_FAIL_DBUS:-0}" = 1 ] \
    && [ "${2:-}" = XDG_CURRENT_DESKTOP ] && [ "${3:-}" = XDG_SESSION_TYPE ]; then
    exit 1
fi
exit 0
SH
cat >"$work/shell" <<'SH'
#!/bin/sh
printf 'shell %s\n' "$*" >>"$TILO_SESSION_TEST_LOG"
printf 'frames=1 direct_frames=0 direct_dropped=0 flip_completions=1 unprofiled_frames=1 window_us=17 bridge_us=11 scanout_us=6 readback_bytes=0 output_bytes=0 DRM render phase summary\n' >&2
if [ -n "${TILO_FRAME_TRACE:-}" ]; then
    printf 'seq,start_us,period_us,input_us,pump_us,layout_us,scene_us,submit_us,testctl_us,clients_us,sleep_us,painted,present_async,input_latency_us,input_retirement_latency_us,backpressured,deadline_late_us\n' >"$TILO_FRAME_TRACE"
    printf '0,0,0,1,2,3,4,5,0,6,7,1,1,8,9,0,10\n' >>"$TILO_FRAME_TRACE"
fi
if [ "${TILO_SESSION_TEST_REQUEST_LOGOUT:-0}" = 1 ]; then
    printf 'logout\n' >"$TILO_SESSION_LOGOUT_REQUEST"
    exit 19
fi
if [ "${TILO_SESSION_TEST_HOLD:-0}" = 1 ]; then
    trap 'printf "shell-term\n" >>"$TILO_SESSION_TEST_LOG"; exit 0' HUP INT TERM
    while :; do sleep 1; done
fi
exit "${TILO_SESSION_TEST_STATUS:-0}"
SH
cat >"$work/systemd-cat" <<'SH'
#!/bin/sh
printf 'systemd-cat %s\n' "$*" >>"$TILO_SESSION_TEST_LOG"
if [ "${2:-}" = --priority=err ]; then
    cat >>"$TILO_SESSION_TEST_LOG"
    exit 0
fi
# The production invocation supplies no command: it is a byte relay into
# journald. Drain it so the real flight recorder never hits a broken pipe.
cat >/dev/null
SH
cat >"$work/loginctl" <<'SH'
#!/bin/sh
printf 'loginctl %s\n' "$*" >>"$TILO_SESSION_TEST_LOG"
exit 0
SH
cp "$repo_root/dist/session/tilo-flight-recorder.py" "$work/tilo-flight-recorder"
chmod 0755 "$work/systemctl" "$work/systemd-cat" "$work/loginctl" \
    "$work/dbus-update-activation-environment" "$work/shell" "$work/tilo-flight-recorder"
tilo_stage_script "$repo_root/dist/session/tilo-session-runner" "$work/tilo-session" \
    SYSTEMCTL="$work/systemctl" \
    SYSTEMD_CAT="$work/systemd-cat" \
    DBUS_UPDATE="$work/dbus-update-activation-environment" \
    LOGINCTL="$work/loginctl" \
    FLIGHT_RECORDER="$work/tilo-flight-recorder"

# The shared runner is an internal package component, but it still fails
# closed when a broken wrapper omits or supplies an unsafe compositor path.
set +e
"$work/tilo-session" >"$work/no-arg.out" 2>&1
no_arg_status=$?
"$work/tilo-session" relative-shell >"$work/relative.out" 2>&1
relative_status=$?
"$work/tilo-session" "$work/missing-shell" >"$work/missing.out" 2>&1
missing_status=$?
chmod 0644 "$work/tilo-flight-recorder"
"$work/tilo-session" "$work/shell" >"$work/missing-recorder.out" 2>&1
missing_recorder_status=$?
chmod 0755 "$work/tilo-flight-recorder"
set -e
[[ $no_arg_status == 2 ]]
[[ $relative_status == 2 ]]
[[ $missing_status == 1 ]]
[[ $missing_recorder_status == 1 ]]
grep -Fqx 'tilo-session-runner: missing compositor path' "$work/no-arg.out"
grep -Fqx 'tilo-session-runner: compositor path must be absolute' "$work/relative.out"
grep -Fqx "tilo-session-runner: $work/missing-shell is missing or not executable" "$work/missing.out"
grep -Fqx "tilo-session: $work/tilo-flight-recorder is missing or not executable" \
    "$work/missing-recorder.out"

# The public production entry must remain non-redirectable. In particular,
# environment names used by an older developer-wrapper prototype cannot turn
# into an authorization channel: only its literal package shell and runner are
# consulted. Source-level assertions are appropriate because their absolute
# paths are replaced by each package format after staging.
production=$repo_root/dist/session/tilo-session
grep -Fqx 'TILO_SHELL=/usr/bin/tilo-shell' "$production"
grep -Fqx 'SESSION_RUNNER=/usr/libexec/tilo-session-runner' "$production"
! grep -Fq 'TILO_SESSION_SHELL' "$production"
! grep -Fq 'TILO_SESSION_DEV_VERIFIED' "$production"

export XDG_RUNTIME_DIR=$work/runtime
mkdir -m 0700 "$XDG_RUNTIME_DIR"
export XDG_STATE_HOME=$work/state
mkdir -p "$XDG_STATE_HOME/tilo/session-logs"
printf 'prior private diagnostic\n' >"$XDG_STATE_HOME/tilo/session-logs/latest.log"
chmod 0644 "$XDG_STATE_HOME/tilo/session-logs/latest.log"
printf 'prior log event\n' >"$XDG_STATE_HOME/tilo/session-logs/events.log"
chmod 0644 "$XDG_STATE_HOME/tilo/session-logs/events.log"
printf 'old frame trace\n' >"$XDG_STATE_HOME/tilo/session-logs/frame-trace.csv"
chmod 0644 "$XDG_STATE_HOME/tilo/session-logs/frame-trace.csv"

set +e
TILO_SESSION_TEST_LOG=$log TILO_SESSION_TEST_STATUS=7 \
    TILO_SESSION_TEST_SECRET='must-not-be-logged' "$work/tilo-session" "$work/shell"
status=$?
set -e
[[ $status == 7 ]]
# A rotated log contains the preceding session's compositor and application
# diagnostics. It must remain private just like the current flight record,
# regardless of the caller's umask or a pre-existing file mode.
flight_dir=${XDG_STATE_HOME:-$HOME/.local/state}/tilo/session-logs
test "$(stat -c %a "$flight_dir/latest.log")" = 600
test "$(stat -c %a "$flight_dir/previous.log")" = 600
grep -Fqx 'prior private diagnostic' "$flight_dir/previous.log"
test "$(stat -c %a "$flight_dir/frame-trace.csv")" = 600
test "$(stat -c %a "$flight_dir/previous-frame-trace.csv")" = 600
grep -Fqx 'old frame trace' "$flight_dir/previous-frame-trace.csv"
grep -Fqx '0,0,0,1,2,3,4,5,0,6,7,1,1,8,9,0,10' "$flight_dir/frame-trace.csv"
grep -Fq 'tilo-flight-record-v1 start=' "$flight_dir/latest.log"
grep -Fq 'DRM render phase summary' "$flight_dir/latest.log"
grep -Fq 'tilo-flight-record-v1 end=' "$flight_dir/latest.log"
# The log events are kept, and rotated, beside the full record.
test "$(stat -c %a "$flight_dir/events.log")" = 600
test "$(stat -c %a "$flight_dir/previous-events.log")" = 600
grep -Fqx 'prior log event' "$flight_dir/previous-events.log"
head -n 1 "$flight_dir/events.log" | grep -Fq 'tilo-flight-record-v1 start='
tail -n 1 "$flight_dir/events.log" | grep -Fq 'tilo-flight-record-v1 end='
cat >"$work/expected" <<EOF
systemctl --user is-active --quiet tilo-graphical-session.target
systemctl --user is-active --quiet graphical-session.target
systemctl --user is-active --quiet graphical-session-pre.target
systemctl --user unset-environment DISPLAY WAYLAND_DISPLAY XAUTHORITY DESKTOP_SESSION GDMSESSION XDG_SESSION_DESKTOP
dbus-update --systemd XDG_CURRENT_DESKTOP XDG_SESSION_TYPE
systemctl --user import-environment XDG_CURRENT_DESKTOP XDG_SESSION_TYPE
systemctl --user start tilo-graphical-session.target
shell --host-desktop-session --firstboot --backend drm
systemd-cat --identifier=tilo-session --level-prefix=false
systemd-cat --identifier=tilo-session --priority=err --level-prefix=false
tilo-session: compositor exited without a logout request (exit status 7)
systemctl --user stop tilo-graphical-session.target
dbus-update --systemd XDG_CURRENT_DESKTOP= XDG_SESSION_TYPE=
systemctl --user unset-environment DISPLAY WAYLAND_DISPLAY XAUTHORITY DESKTOP_SESSION GDMSESSION XDG_CURRENT_DESKTOP XDG_SESSION_DESKTOP XDG_SESSION_TYPE
EOF
# The FIFO relay and compositor are deliberately concurrent; either may append
# its startup breadcrumb first. Compare the lifecycle as a set here, then keep
# the ordered assertions below for teardown and failure handling.
sort "$work/expected" >"$work/expected.sorted"
sort "$log" >"$work/log.sorted"
cmp "$work/expected.sorted" "$work/log.sorted"
! grep -Fq 'must-not-be-logged' "$log"

: >"$log"
set +e
TILO_SESSION_TEST_LOG=$log TILO_SESSION_TEST_PREDECESSOR=1 "$work/tilo-session" "$work/shell"
status=$?
set -e
[[ $status == 1 ]]
cat >"$work/expected-predecessor" <<EOF
systemctl --user is-active --quiet tilo-graphical-session.target
systemctl --user is-active --quiet graphical-session.target
systemctl --user stop tilo-graphical-session.target graphical-session.target graphical-session-pre.target
systemctl --user unset-environment DISPLAY WAYLAND_DISPLAY XAUTHORITY DESKTOP_SESSION GDMSESSION XDG_SESSION_DESKTOP
dbus-update --systemd XDG_CURRENT_DESKTOP XDG_SESSION_TYPE
systemctl --user import-environment XDG_CURRENT_DESKTOP XDG_SESSION_TYPE
systemctl --user start tilo-graphical-session.target
shell --host-desktop-session --firstboot --backend drm
systemd-cat --identifier=tilo-session --level-prefix=false
systemd-cat --identifier=tilo-session --priority=err --level-prefix=false
tilo-session: compositor exited without a logout request (exit status 0)
systemctl --user stop tilo-graphical-session.target
dbus-update --systemd XDG_CURRENT_DESKTOP= XDG_SESSION_TYPE=
systemctl --user unset-environment DISPLAY WAYLAND_DISPLAY XAUTHORITY DESKTOP_SESSION GDMSESSION XDG_CURRENT_DESKTOP XDG_SESSION_DESKTOP XDG_SESSION_TYPE
EOF
sort "$work/expected-predecessor" >"$work/expected-predecessor.sorted"
sort "$log" >"$work/log.sorted"
cmp "$work/expected-predecessor.sorted" "$work/log.sorted"

: >"$log"
TILO_SESSION_TEST_LOG=$log TILO_SESSION_TEST_HOLD=1 "$work/tilo-session" "$work/shell" &
wrapper_pid=$!
for _ in $(seq 1 100); do
    grep -Fq 'shell --host-desktop-session' "$log" 2>/dev/null && break
    sleep 0.02
done
grep -Fq 'shell --host-desktop-session' "$log"
kill -TERM "$wrapper_pid"
wait "$wrapper_pid" || [[ $? == 143 ]]
grep -Fqx 'shell-term' "$log"
[[ $(grep -Fc 'systemctl --user stop tilo-graphical-session.target' "$log") == 1 ]]

# Shell-requested logout must return success through the wrapper-owned cleanup
# path, even if the shell uses a non-zero sentinel while exiting. Most
# importantly, target/environment teardown happens exactly once rather than
# being killed alongside `loginctl terminate-session self`.
: >"$log"
XDG_SESSION_ID=fixture-42 TILO_SESSION_TEST_LOG=$log TILO_SESSION_TEST_REQUEST_LOGOUT=1 \
    "$work/tilo-session" "$work/shell"
! grep -Fq 'compositor exited without a logout request' "$log"
grep -Fqx 'systemctl --user stop tilo-graphical-session.target' "$log"
grep -Fqx 'dbus-update --systemd XDG_CURRENT_DESKTOP= XDG_SESSION_TYPE=' "$log"
grep -Fqx 'systemctl --user unset-environment DISPLAY WAYLAND_DISPLAY XAUTHORITY DESKTOP_SESSION GDMSESSION XDG_CURRENT_DESKTOP XDG_SESSION_DESKTOP XDG_SESSION_TYPE' "$log"
[[ $(grep -Fc 'systemctl --user stop tilo-graphical-session.target' "$log") == 1 ]]
grep -Fqx 'loginctl terminate-session fixture-42' "$log"
if find "$XDG_RUNTIME_DIR/tilo-session" -name 'logout-request-*' -print -quit | grep -q .; then
    echo 'logout request leaked after wrapper cleanup' >&2
    exit 1
fi

: >"$log"
set +e
TILO_SESSION_TEST_LOG=$log TILO_SESSION_TEST_FAIL_START=1 "$work/tilo-session" "$work/shell"
status=$?
set -e
[[ $status == 1 ]]
grep -Fqx 'dbus-update --systemd XDG_CURRENT_DESKTOP= XDG_SESSION_TYPE=' "$log"
grep -Fqx 'systemctl --user unset-environment DISPLAY WAYLAND_DISPLAY XAUTHORITY DESKTOP_SESSION GDMSESSION XDG_CURRENT_DESKTOP XDG_SESSION_DESKTOP XDG_SESSION_TYPE' "$log"
grep -Fqx 'systemd-cat --identifier=tilo-session --priority=err --level-prefix=false' "$log"
grep -Fqx 'tilo-session: startup failed during Tilo graphical session target start (exit status 1)' "$log"
! grep -Fq 'shell --host-desktop-session' "$log"

for failure in dbus import; do
    : >"$log"
    set +e
    case "$failure" in
        dbus) TILO_SESSION_TEST_LOG=$log TILO_SESSION_TEST_FAIL_DBUS=1 "$work/tilo-session" "$work/shell" ;;
        import) TILO_SESSION_TEST_LOG=$log TILO_SESSION_TEST_FAIL_IMPORT=1 "$work/tilo-session" "$work/shell" ;;
    esac
    status=$?
    set -e
    [[ $status == 1 ]]
    grep -Fqx 'systemd-cat --identifier=tilo-session --priority=err --level-prefix=false' "$log"
    case "$failure" in
        dbus)
            grep -Fqx 'tilo-session: startup failed during D-Bus activation environment update (exit status 1)' "$log"
            ;;
        import)
            grep -Fqx 'tilo-session: startup failed during systemd user environment import (exit status 1)' "$log"
            ;;
    esac
    grep -Fqx 'dbus-update --systemd XDG_CURRENT_DESKTOP= XDG_SESSION_TYPE=' "$log"
    grep -Fqx 'systemctl --user unset-environment DISPLAY WAYLAND_DISPLAY XAUTHORITY DESKTOP_SESSION GDMSESSION XDG_CURRENT_DESKTOP XDG_SESSION_DESKTOP XDG_SESSION_TYPE' "$log"
    ! grep -Fq 'shell --host-desktop-session' "$log"
done

echo 'host session lifecycle: PASS'
