#!/usr/bin/env bash
# Source/runtime contract for the greetd -> systemd user target -> verified
# identity -> compositor lifecycle. Uses a rewritten temporary copy so the
# production wrapper keeps absolute, non-overridable trusted command paths.
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
wrapper="$repo/os/usr/libexec/tilo-session"
target="$repo/os/usr/lib/systemd/user/tilo-graphical-session.target"
selinux_policy="$repo/os/usr/share/tilo/selinux/tilo-base.cil"
identity_unit="$repo/os/usr/lib/systemd/user/tilo-identity.service"
recipient_unit="$repo/os/usr/lib/systemd/user/tilo-fs-recipient.service"
recipient_socket="$repo/os/usr/lib/systemd/user/tilo-fs-recipient.socket"
release_workflow="$repo/.forgejo/workflows/build-image.yml"
shell_main="$repo/crates/session/shell/src/linux_main.rs"

grep -Fqx 'Requires=graphical-session-pre.target graphical-session.target' "$target"
grep -Fqx 'After=graphical-session-pre.target' "$target"
grep -Fqx 'PartOf=graphical-session.target' "$target"
grep -Fqx 'PropagatesStopTo=graphical-session.target graphical-session-pre.target' "$target"
grep -Fqx 'Before=graphical-session-pre.target' "$identity_unit"
grep -Fqx 'WantedBy=graphical-session-pre.target' "$identity_unit"
grep -Fqx 'ExecStart=/usr/bin/shell --provision-identity' "$identity_unit"
grep -Fqx 'ConditionPathExists=%E/tilo/identity/primary.json' "$identity_unit"
grep -Fqx 'ConditionPathExists=!%E/tilo/identity/.chain-v1-repaired' "$identity_unit"
grep -Fqx 'ExecStartPost=/usr/bin/mkdir -p %E/tilo/identity' "$identity_unit"
grep -Fqx 'ExecStartPost=/usr/bin/touch %E/tilo/identity/.chain-v1-repaired' "$identity_unit"
grep -Fqx 'LimitCORE=0' "$identity_unit"
grep -Fqx 'ulimit -c 0' "$wrapper"
for policy in \
  'ProtectSystem=strict' \
  'ProtectHome=read-only' \
  'ReadWritePaths=-%E/tilo/identity /var/lib/tilo/user-identity /var/lib/tilo/device' \
  'PrivateDevices=yes' \
  'PrivateIPC=yes' \
  'PrivateNetwork=yes' \
  'RestrictNamespaces=yes' \
  'MemoryDenyWriteExecute=yes' \
  'CapabilityBoundingSet=' \
  'AmbientCapabilities=' \
  'KeyringMode=private' \
  'SystemCallFilter=@system-service' \
  'SystemCallFilter=~@clock @debug @keyring @module @mount @obsolete @raw-io @reboot @resources @swap'; do
  grep -Fqx "$policy" "$identity_unit"
done
grep -Fq 'tilo_session_unit_file_t' "$selinux_policy"
grep -Fqx '(allow xdm_t tilo_session_unit_file_t (service (start stop status)))' "$selinux_policy"
if grep -q '^SuccessExitStatus=' "$identity_unit"; then
  echo 'identity migration must not turn failure into successful enrollment' >&2
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
  'ReadWritePaths=-%h/Shared -%h/.cache/tilo/received-files %t/tilo' \
  'PrivateIPC=yes' \
  'ProtectKernelLogs=yes' \
  'ProtectClock=yes' \
  'ProtectHostname=yes' \
  'RestrictRealtime=yes' \
  'KeyringMode=private' \
  'RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6'; do
  grep -Fqx "$policy" "$recipient_unit"
done
grep -Fqx 'Exec=/usr/libexec/tilo-session --backend drm' \
  "$repo/os/usr/share/wayland-sessions/shell.desktop"
grep -Fqx 'Exec=/usr/libexec/tilo-session --backend drm --enable-peer-sync' \
  "$repo/os/usr/share/wayland-sessions/shell-peer-sync.desktop"
grep -Fq "pgrep -f '^/usr/bin/shell --backend drm( |$)'" "$release_workflow"
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
fake_shell="$tmp/shell"
test_wrapper="$tmp/tilo-session"
runtime_dir="$tmp/runtime"
install -d -m 700 "$runtime_dir"

cat >"$fake_systemctl" <<'SH'
#!/bin/bash
printf 'systemctl %s\n' "$*" >>"$TILO_SESSION_TEST_LOG"
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
printf 'desktop %s\n' "$*" >>"$TILO_SESSION_TEST_LOG"
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
if [ "${TILO_SESSION_HOLD:-0}" = 1 ]; then
  trap 'printf "desktop-term\n" >>"$TILO_SESSION_TEST_LOG"; exit 0' TERM HUP INT
  while :; do sleep 1; done
fi
exit "${TILO_SESSION_DESKTOP_STATUS:-0}"
SH
chmod 0755 "$fake_systemctl" "$fake_shell"
sed \
  -e "s|readonly SYSTEMCTL=/usr/bin/systemctl|readonly SYSTEMCTL=$fake_systemctl|" \
  -e "s|readonly SHELL_BIN=/usr/bin/shell|readonly SHELL_BIN=$fake_shell|" \
  "$wrapper" >"$test_wrapper"
chmod 0755 "$test_wrapper"
grep -Fqx "readonly SYSTEMCTL=$fake_systemctl" "$test_wrapper"
grep -Fqx "readonly SHELL_BIN=$fake_shell" "$test_wrapper"
! grep -Fq '/usr/bin/systemctl' "$test_wrapper"
! grep -Fq '/usr/bin/shell' "$test_wrapper"

# Successful verification starts the desktop, preserves its exit status, and
# tears down the Tilo target afterward.
set +e
XDG_RUNTIME_DIR="$runtime_dir" TILO_SESSION_TEST_LOG="$log" TILO_SESSION_DESKTOP_STATUS=7 \
  "$test_wrapper" --backend drm
status=$?
set -e
[ "$status" = 7 ]
cat >"$tmp/expected" <<'EOF'
systemctl --user start tilo-graphical-session.target
desktop --backend drm
systemctl --user stop tilo-graphical-session.target
EOF
cmp "$tmp/expected" "$log"

# Logout gets a private absolute channel from the wrapper, and the wrapper
# removes both the request file and its private directory after the compositor
# returns. This is the cross-process edge used by os-actions::Logout.
: >"$log"
XDG_RUNTIME_DIR="$runtime_dir" TILO_SESSION_TEST_LOG="$log" TILO_SESSION_WRITE_LOGOUT=1 \
  "$test_wrapper" --backend drm
cat >"$tmp/expected" <<'EOF'
systemctl --user start tilo-graphical-session.target
desktop --backend drm
systemctl --user stop tilo-graphical-session.target
EOF
cmp "$tmp/expected" "$log"
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
systemctl --user start tilo-graphical-session.target
desktop --backend drm
systemctl --user stop tilo-graphical-session.target
EOF
cmp "$tmp/expected" "$log"

# Session termination is forwarded to the compositor and cleanup still stops
# the lifecycle target.
: >"$log"
rm -f "$log.verify-count"
XDG_RUNTIME_DIR="$runtime_dir" TILO_SESSION_TEST_LOG="$log" TILO_SESSION_HOLD=1 \
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
[ "$(grep -Fc 'systemctl --user stop tilo-graphical-session.target' "$log")" = 1 ]

printf 'Tilo session lifecycle contract: PASS\n'
