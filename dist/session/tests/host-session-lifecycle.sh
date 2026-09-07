#!/usr/bin/env bash
# Exercise the packaged display-manager wrapper without touching the caller's
# real user manager or graphical session.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
work=$(mktemp -d)
cleanup() { rm -rf -- "$work"; }
trap cleanup EXIT

log=$work/log
cat >"$work/systemctl" <<'SH'
#!/bin/sh
printf 'systemctl %s\n' "$*" >>"$QOS_SESSION_TEST_LOG"
if [ "${QOS_SESSION_TEST_FAIL_IMPORT:-0}" = 1 ] && [ "${2:-}" = import-environment ]; then
    exit 1
fi
if [ "${QOS_SESSION_TEST_FAIL_START:-0}" = 1 ] && [ "${2:-}" = start ]; then
    exit 1
fi
if [ "${2:-}" = is-active ]; then
    if [ "${QOS_SESSION_TEST_PREDECESSOR:-0}" = 1 ] \
        && [ "${4:-}" = graphical-session.target ]; then
        exit 0
    fi
    exit 3
fi
exit 0
SH
cat >"$work/dbus-update-activation-environment" <<'SH'
#!/bin/sh
printf 'dbus-update %s\n' "$*" >>"$QOS_SESSION_TEST_LOG"
if [ "${QOS_SESSION_TEST_FAIL_DBUS:-0}" = 1 ] \
    && [ "${2:-}" = XDG_CURRENT_DESKTOP ] && [ "${3:-}" = XDG_SESSION_TYPE ]; then
    exit 1
fi
exit 0
SH
cat >"$work/shell" <<'SH'
#!/bin/sh
printf 'shell %s\n' "$*" >>"$QOS_SESSION_TEST_LOG"
if [ "${QOS_SESSION_TEST_HOLD:-0}" = 1 ]; then
    trap 'printf "shell-term\n" >>"$QOS_SESSION_TEST_LOG"; exit 0' HUP INT TERM
    while :; do sleep 1; done
fi
exit "${QOS_SESSION_TEST_STATUS:-0}"
SH
cat >"$work/systemd-cat" <<'SH'
#!/bin/sh
printf 'systemd-cat %s\n' "$*" >>"$QOS_SESSION_TEST_LOG"
if [ "${2:-}" = --priority=err ]; then
    cat >>"$QOS_SESSION_TEST_LOG"
    exit 0
fi
shift 2
exec "$@"
SH
chmod 0755 "$work/systemctl" "$work/systemd-cat" \
    "$work/dbus-update-activation-environment" "$work/shell"
sed \
    -e "s|QOS_SHELL=/usr/bin/qos-shell|QOS_SHELL=$work/shell|" \
    -e "s|SYSTEMCTL=/usr/bin/systemctl|SYSTEMCTL=$work/systemctl|" \
    -e "s|SYSTEMD_CAT=/usr/bin/systemd-cat|SYSTEMD_CAT=$work/systemd-cat|" \
    -e "s|DBUS_UPDATE=/usr/bin/dbus-update-activation-environment|DBUS_UPDATE=$work/dbus-update-activation-environment|" \
    "$repo_root/dist/session/qos-session" >"$work/qos-session"
chmod 0755 "$work/qos-session"

set +e
QOS_SESSION_TEST_LOG=$log QOS_SESSION_TEST_STATUS=7 \
    QOS_SESSION_TEST_SECRET='must-not-be-logged' "$work/qos-session"
status=$?
set -e
[[ $status == 7 ]]
cat >"$work/expected" <<EOF
systemctl --user is-active --quiet qos-graphical-session.target
systemctl --user is-active --quiet graphical-session.target
systemctl --user is-active --quiet graphical-session-pre.target
systemctl --user unset-environment DISPLAY WAYLAND_DISPLAY XAUTHORITY DESKTOP_SESSION GDMSESSION XDG_SESSION_DESKTOP
dbus-update --systemd XDG_CURRENT_DESKTOP XDG_SESSION_TYPE
systemctl --user import-environment XDG_CURRENT_DESKTOP XDG_SESSION_TYPE
systemctl --user start qos-graphical-session.target
systemd-cat --identifier=qos-session --level-prefix=false $work/shell --host-desktop-session --firstboot --backend drm
shell --host-desktop-session --firstboot --backend drm
systemctl --user stop qos-graphical-session.target
dbus-update --systemd XDG_CURRENT_DESKTOP= XDG_SESSION_TYPE=
systemctl --user unset-environment DISPLAY WAYLAND_DISPLAY XAUTHORITY DESKTOP_SESSION GDMSESSION XDG_CURRENT_DESKTOP XDG_SESSION_DESKTOP XDG_SESSION_TYPE
EOF
cmp "$work/expected" "$log"
! grep -Fq 'must-not-be-logged' "$log"

: >"$log"
QOS_SESSION_TEST_LOG=$log QOS_SESSION_TEST_PREDECESSOR=1 "$work/qos-session"
cat >"$work/expected-predecessor" <<EOF
systemctl --user is-active --quiet qos-graphical-session.target
systemctl --user is-active --quiet graphical-session.target
systemctl --user stop qos-graphical-session.target graphical-session.target graphical-session-pre.target
systemctl --user unset-environment DISPLAY WAYLAND_DISPLAY XAUTHORITY DESKTOP_SESSION GDMSESSION XDG_SESSION_DESKTOP
dbus-update --systemd XDG_CURRENT_DESKTOP XDG_SESSION_TYPE
systemctl --user import-environment XDG_CURRENT_DESKTOP XDG_SESSION_TYPE
systemctl --user start qos-graphical-session.target
systemd-cat --identifier=qos-session --level-prefix=false $work/shell --host-desktop-session --firstboot --backend drm
shell --host-desktop-session --firstboot --backend drm
systemctl --user stop qos-graphical-session.target
dbus-update --systemd XDG_CURRENT_DESKTOP= XDG_SESSION_TYPE=
systemctl --user unset-environment DISPLAY WAYLAND_DISPLAY XAUTHORITY DESKTOP_SESSION GDMSESSION XDG_CURRENT_DESKTOP XDG_SESSION_DESKTOP XDG_SESSION_TYPE
EOF
cmp "$work/expected-predecessor" "$log"

: >"$log"
QOS_SESSION_TEST_LOG=$log QOS_SESSION_TEST_HOLD=1 "$work/qos-session" &
wrapper_pid=$!
for _ in $(seq 1 100); do
    grep -Fq 'shell --host-desktop-session' "$log" 2>/dev/null && break
    sleep 0.02
done
grep -Fq 'shell --host-desktop-session' "$log"
kill -TERM "$wrapper_pid"
wait "$wrapper_pid" || [[ $? == 143 ]]
grep -Fqx 'shell-term' "$log"
[[ $(grep -Fc 'systemctl --user stop qos-graphical-session.target' "$log") == 1 ]]

: >"$log"
set +e
QOS_SESSION_TEST_LOG=$log QOS_SESSION_TEST_FAIL_START=1 "$work/qos-session"
status=$?
set -e
[[ $status == 1 ]]
grep -Fqx 'dbus-update --systemd XDG_CURRENT_DESKTOP= XDG_SESSION_TYPE=' "$log"
grep -Fqx 'systemctl --user unset-environment DISPLAY WAYLAND_DISPLAY XAUTHORITY DESKTOP_SESSION GDMSESSION XDG_CURRENT_DESKTOP XDG_SESSION_DESKTOP XDG_SESSION_TYPE' "$log"
grep -Fqx 'systemd-cat --identifier=qos-session --priority=err --level-prefix=false' "$log"
grep -Fqx 'qos-session: startup failed during qOS graphical session target start (exit status 1)' "$log"
! grep -Fq "systemd-cat --identifier=qos-session --level-prefix=false $work/shell" "$log"

for failure in dbus import; do
    : >"$log"
    set +e
    case "$failure" in
        dbus) QOS_SESSION_TEST_LOG=$log QOS_SESSION_TEST_FAIL_DBUS=1 "$work/qos-session" ;;
        import) QOS_SESSION_TEST_LOG=$log QOS_SESSION_TEST_FAIL_IMPORT=1 "$work/qos-session" ;;
    esac
    status=$?
    set -e
    [[ $status == 1 ]]
    grep -Fqx 'systemd-cat --identifier=qos-session --priority=err --level-prefix=false' "$log"
    case "$failure" in
        dbus)
            grep -Fqx 'qos-session: startup failed during D-Bus activation environment update (exit status 1)' "$log"
            ;;
        import)
            grep -Fqx 'qos-session: startup failed during systemd user environment import (exit status 1)' "$log"
            ;;
    esac
    grep -Fqx 'dbus-update --systemd XDG_CURRENT_DESKTOP= XDG_SESSION_TYPE=' "$log"
    grep -Fqx 'systemctl --user unset-environment DISPLAY WAYLAND_DISPLAY XAUTHORITY DESKTOP_SESSION GDMSESSION XDG_CURRENT_DESKTOP XDG_SESSION_DESKTOP XDG_SESSION_TYPE' "$log"
    ! grep -Fq 'shell --host-desktop-session' "$log"
done

echo 'host session lifecycle: PASS'
