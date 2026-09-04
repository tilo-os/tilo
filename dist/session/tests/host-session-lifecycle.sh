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
chmod 0755 "$work/systemctl" "$work/shell"
sed \
    -e "s|QOS_SHELL=/usr/bin/qos-shell|QOS_SHELL=$work/shell|" \
    -e "s|SYSTEMCTL=/usr/bin/systemctl|SYSTEMCTL=$work/systemctl|" \
    "$repo_root/dist/session/qos-session" >"$work/qos-session"
chmod 0755 "$work/qos-session"

set +e
QOS_SESSION_TEST_LOG=$log QOS_SESSION_TEST_STATUS=7 "$work/qos-session"
status=$?
set -e
[[ $status == 7 ]]
cat >"$work/expected" <<'EOF'
systemctl --user unset-environment DISPLAY WAYLAND_DISPLAY XAUTHORITY DESKTOP_SESSION GDMSESSION XDG_SESSION_DESKTOP
systemctl --user import-environment XDG_CURRENT_DESKTOP XDG_SESSION_TYPE
systemctl --user start qos-graphical-session.target
shell --host-desktop-session --firstboot --backend drm
systemctl --user stop qos-graphical-session.target
systemctl --user unset-environment XDG_CURRENT_DESKTOP XDG_SESSION_TYPE
EOF
cmp "$work/expected" "$log"

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

echo 'host session lifecycle: PASS'
