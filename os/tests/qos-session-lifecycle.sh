#!/usr/bin/env bash
# Source/runtime contract for the greetd -> systemd user target -> verified
# identity -> compositor lifecycle. Uses a rewritten temporary copy so the
# production wrapper keeps absolute, non-overridable trusted command paths.
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
wrapper="$repo/os/usr/libexec/qos-session"
target="$repo/os/usr/lib/systemd/user/qos-graphical-session.target"
selinux_policy="$repo/os/usr/share/qos/selinux/qos-base.cil"
identity_unit="$repo/os/usr/lib/systemd/user/qos-identity.service"
recipient_unit="$repo/os/usr/lib/systemd/user/qos-fs-recipient.service"
release_workflow="$repo/.forgejo/workflows/build-image.yml"

grep -Fqx 'Requires=graphical-session-pre.target graphical-session.target' "$target"
grep -Fqx 'After=graphical-session-pre.target' "$target"
grep -Fqx 'PartOf=graphical-session.target' "$target"
grep -Fqx 'PropagatesStopTo=graphical-session.target graphical-session-pre.target' "$target"
grep -Fqx 'Before=graphical-session-pre.target' "$identity_unit"
grep -Fqx 'WantedBy=graphical-session-pre.target' "$identity_unit"
grep -Fqx 'ExecStart=/usr/bin/shell --provision-identity' "$identity_unit"
grep -Fq 'qos_session_unit_file_t' "$selinux_policy"
grep -Fqx '(allow xdm_t qos_session_unit_file_t (service (start stop status)))' "$selinux_policy"
if grep -q '^SuccessExitStatus=' "$identity_unit"; then
  echo 'identity migration must not turn failure into successful enrollment' >&2
  exit 1
fi
grep -Fqx 'PartOf=graphical-session.target' "$recipient_unit"
grep -Fqx 'WantedBy=graphical-session.target' "$recipient_unit"
grep -Fqx 'Exec=/usr/libexec/qos-session --backend drm' \
  "$repo/os/usr/share/wayland-sessions/shell.desktop"
grep -Fqx 'Exec=/usr/libexec/qos-session --backend drm --enable-peer-sync' \
  "$repo/os/usr/share/wayland-sessions/shell-peer-sync.desktop"
grep -Fq "pgrep -f '^/usr/bin/shell --backend drm( |$)'" "$release_workflow"
grep -Fq './os/dev/vm.sh keys tab tab ret' "$release_workflow"
grep -Fq './os/dev/vm.sh keys tab spc' "$release_workflow"
grep -Fq './os/dev/vm.sh keys tab ret' "$release_workflow"
grep -Fq 'identity ceremony did not produce the certified chain' "$release_workflow"

tmp_root="${TMPDIR:-/tmp}"
tmp=$(mktemp -d "$tmp_root/qos-session-lifecycle.XXXXXX")
cleanup() {
  case "$tmp" in "$tmp_root"/qos-session-lifecycle.*) rm -r "$tmp" ;; esac
}
trap cleanup EXIT INT TERM

log="$tmp/log"
fake_systemctl="$tmp/systemctl"
fake_shell="$tmp/shell"
test_wrapper="$tmp/qos-session"

cat >"$fake_systemctl" <<'SH'
#!/bin/bash
printf 'systemctl %s\n' "$*" >>"$QOS_SESSION_TEST_LOG"
exit 0
SH
cat >"$fake_shell" <<'SH'
#!/bin/bash
if [ "${1:-}" = --verify-collaboration-identity ]; then
  printf 'verify\n' >>"$QOS_SESSION_TEST_LOG"
  count_file="$QOS_SESSION_TEST_LOG.verify-count"
  count=0
  [ ! -f "$count_file" ] || count=$(cat "$count_file")
  count=$((count + 1))
  printf '%s\n' "$count" >"$count_file"
  sequence="${QOS_SESSION_VERIFY_SEQUENCE:-0}"
  status=$(printf '%s\n' "$sequence" | cut -d, -f"$count")
  [ -n "$status" ] || status=$(printf '%s\n' "$sequence" | awk -F, '{print $NF}')
  exit "$status"
fi
if [ "${1:-}" = --provision-identity ]; then
  printf 'provision\n' >>"$QOS_SESSION_TEST_LOG"
  exit "${QOS_SESSION_PROVISION_STATUS:-0}"
fi
printf 'desktop %s\n' "$*" >>"$QOS_SESSION_TEST_LOG"
if [ "${QOS_SESSION_HOLD:-0}" = 1 ]; then
  trap 'printf "desktop-term\n" >>"$QOS_SESSION_TEST_LOG"; exit 0' TERM HUP INT
  while :; do sleep 1; done
fi
exit "${QOS_SESSION_DESKTOP_STATUS:-0}"
SH
chmod 0755 "$fake_systemctl" "$fake_shell"
sed \
  -e "s|readonly SYSTEMCTL=/usr/bin/systemctl|readonly SYSTEMCTL=$fake_systemctl|" \
  -e "s|readonly SHELL_BIN=/usr/bin/shell|readonly SHELL_BIN=$fake_shell|" \
  "$wrapper" >"$test_wrapper"
chmod 0755 "$test_wrapper"

# Successful verification starts the desktop, preserves its exit status, and
# tears down the qOS target afterward.
set +e
QOS_SESSION_TEST_LOG="$log" QOS_SESSION_DESKTOP_STATUS=7 \
  "$test_wrapper" --backend drm
status=$?
set -e
[ "$status" = 7 ]
cat >"$tmp/expected" <<'EOF'
systemctl --user start qos-graphical-session.target
desktop --backend drm
systemctl --user stop qos-graphical-session.target
EOF
cmp "$tmp/expected" "$log"

# A missing identity reaches the native compositor, which owns the interactive
# first-login OOBE. The wrapper must not preflight an identity that only that
# compositor can create.
: >"$log"
QOS_SESSION_TEST_LOG="$log" QOS_SESSION_VERIFY_SEQUENCE=1 \
  "$test_wrapper" --backend drm
cat >"$tmp/expected" <<'EOF'
systemctl --user start qos-graphical-session.target
desktop --backend drm
systemctl --user stop qos-graphical-session.target
EOF
cmp "$tmp/expected" "$log"

# Session termination is forwarded to the compositor and cleanup still stops
# the lifecycle target.
: >"$log"
rm -f "$log.verify-count"
QOS_SESSION_TEST_LOG="$log" QOS_SESSION_HOLD=1 \
  "$test_wrapper" --backend drm &
session_pid=$!
for _ in $(seq 1 100); do
  grep -Fq 'desktop --backend drm' "$log" 2>/dev/null && break
  sleep 0.02
done
grep -Fq 'desktop --backend drm' "$log"
kill -TERM "$session_pid"
wait "$session_pid" || [ "$?" = 143 ]
grep -Fqx 'desktop-term' "$log"
[ "$(grep -Fc 'systemctl --user stop qos-graphical-session.target' "$log")" = 1 ]

printf 'qOS session lifecycle contract: PASS\n'
