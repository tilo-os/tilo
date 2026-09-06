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
if [ "${QOS_SESSION_TEST_FAIL_START:-0}" = 1 ] && [ "${2:-}" = start ]; then
    exit 1
fi
exit 0
SH
cat >"$work/dbus-update-activation-environment" <<'SH'
#!/bin/sh
printf 'dbus-update %s\n' "$*" >>"$QOS_SESSION_TEST_LOG"
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
! grep -Fq 'systemd-cat ' "$log"

echo 'host session lifecycle: PASS'
