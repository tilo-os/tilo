#!/usr/bin/env bash
# Source/runtime contract for the greetd -> systemd user target -> verified
# identity -> compositor lifecycle. Uses a rewritten temporary copy so the
# production wrapper keeps absolute, non-overridable trusted command paths.
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
. "$repo/tools/desktop-isolation.sh"
tilo_test_isolate "$@"
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

grep -Fqx 'Requires=graphical-session-pre.target graphical-session.target' "$target"
grep -Fqx 'After=graphical-session-pre.target' "$target"
grep -Fqx 'PartOf=graphical-session.target' "$target"
grep -Fqx 'PropagatesStopTo=graphical-session.target graphical-session-pre.target' "$target"
grep -Fqx 'Wants=tilo-ush.service' "$target"
for socket in \
  "$repo/os/usr/lib/systemd/user/tilo-models.socket" \
  "$repo/os/usr/lib/systemd/user/tilo-model-gateway.socket"; do
  grep -Fqx 'PartOf=tilo-graphical-session.target' "$socket"
  grep -Fqx 'WantedBy=tilo-graphical-session.target' "$socket"
done
grep -Fqx 'ulimit -c 0' "$wrapper"
grep -Fq 'tilo_session_unit_file_t' "$selinux_policy"
grep -Fqx '(allow xdm_t tilo_session_unit_file_t (service (start stop status)))' "$selinux_policy"
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
# Host units keep the mount-namespace sandbox. A tilo-vm slot cannot mount a
# fresh /dev/mqueue; that exception is a drop-in shipped only in os/dev-image.
# Empty assignments do not unset boolean keys (systemd ignores them as
# invalid), so a drop-in must assign false. ReadWritePaths= is a path list:
# an empty assignment resets it, and the merged list must stay empty or
# systemd still enters a mount namespace. PrivateIPC=yes is what mounts
# /dev/mqueue inside that namespace.
dropin_root="$repo/os/dev-image/usr/lib/systemd/user"
# Boolean keys whose empty assignment systemd rejects, leaving production on.
nested_bools='ProtectSystem|ProtectHome|ProtectKernelTunables|ProtectKernelModules|ProtectKernelLogs|PrivateIPC|PrivateTmp|ProtectControlGroups'
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
  dropin="$dropin_root/$unit.d/10-nested-user-namespace.conf"
  test -f "$dropin" || {
    echo "missing nested-session drop-in for $unit" >&2
    exit 1
  }
  for off in \
    ProtectSystem=false \
    ProtectHome=false \
    ProtectKernelTunables=false \
    ProtectKernelModules=false \
    ProtectKernelLogs=false \
    PrivateIPC=false \
    PrivateTmp=false \
    ProtectControlGroups=false \
    ReadWritePaths=; do
    grep -Fqx "$off" "$dropin" || {
      echo "$dropin does not turn off $off" >&2
      exit 1
    }
  done
  if grep -Eq "^(${nested_bools})=\$" "$dropin"; then
    echo "$dropin still uses an empty boolean assignment" >&2
    exit 1
  fi
  if grep -Eq '^(ProtectClock|ProtectHostname)=' "$dropin"; then
    echo "$dropin overrides ProtectClock or ProtectHostname" >&2
    exit 1
  fi
  # Last non-empty assignment wins. An empty boolean is ignored, so
  # PrivateIPC= in the drop-in leaves production PrivateIPC=yes in force.
  for key in PrivateIPC PrivateTmp ProtectControlGroups; do
    merged=""
    while IFS= read -r line; do
      case "$line" in
        "$key"=) ;;
        "$key"=*) merged="${line#"$key"=}" ;;
      esac
    done < <(grep -h -E "^${key}=" "$prod" "$dropin" || true)
    if [ "$merged" = yes ]; then
      echo "$unit dev-image merge still sets ${key}=yes" >&2
      exit 1
    fi
  done
  merged_rw=""
  while IFS= read -r line; do
    value="${line#ReadWritePaths=}"
    if [ -z "$value" ]; then
      merged_rw=""
    else
      merged_rw="${merged_rw:+$merged_rw }$value"
    fi
  done < <(grep -h -E '^ReadWritePaths=' "$prod" "$dropin" || true)
  if [ -n "$merged_rw" ]; then
    echo "$unit dev-image merge still sets ReadWritePaths=$merged_rw" >&2
    exit 1
  fi
done
if grep -R -F -- '10-nested-user-namespace.conf' "$repo/os/Containerfile" "$repo/os/usr" >/dev/null; then
  echo 'nested-session drop-in leaked into the production image' >&2
  exit 1
fi
command -v systemd-analyze >/dev/null || {
  echo 'systemd-analyze is required to prove the nested drop-in parses' >&2
  exit 1
}
(
  verify_root=$(mktemp -d)
  trap 'rm -rf "$verify_root"' EXIT
  mkdir -p "$verify_root/etc" "$verify_root/usr/lib/systemd/system"
  printf 'NAME=Test\nID=test\nVERSION_ID=1\n' >"$verify_root/etc/os-release"
  printf 'NAME=Test\nID=test\nVERSION_ID=1\n' >"$verify_root/usr/lib/os-release"
  for unit in tilo-fs-recipient.service tilo-fs-cloud.service tilo-models.service; do
    cp "$repo/os/usr/lib/systemd/user/$unit" "$verify_root/usr/lib/systemd/system/$unit"
    mkdir -p "$verify_root/usr/lib/systemd/system/$unit.d"
    cp "$dropin_root/$unit.d/10-nested-user-namespace.conf" \
      "$verify_root/usr/lib/systemd/system/$unit.d/"
  done
  verify_log=$(mktemp)
  systemd-analyze --root="$verify_root" verify \
    tilo-fs-recipient.service tilo-fs-cloud.service tilo-models.service \
    >"$verify_log" 2>&1 || true
  if grep -E 'Failed to parse (Protect|PrivateIPC|PrivateTmp|ReadWritePaths)' "$verify_log"; then
    echo 'systemd-analyze rejected a nested-session sandbox drop-in' >&2
    cat "$verify_log" >&2
    rm -f "$verify_log"
    exit 1
  fi
  rm -f "$verify_log"
)
grep -Fqx 'Exec=/usr/libexec/tilo-session --backend drm' \
  "$repo/os/usr/share/wayland-sessions/shell.desktop"
grep -Fqx 'Exec=/usr/libexec/tilo-session --backend drm --enable-peer-sync' \
  "$repo/os/usr/share/wayland-sessions/shell-peer-sync.desktop"
grep -Fq 'missing="$(ldd /usr/bin/shell | awk '\''/not found/ { print $1 }'\'')"' \
  "$repo/os/Containerfile"
grep -Fq 'if [ -n "$missing" ]; then' "$repo/os/Containerfile"
grep -Fq 'shell has unresolved runtime libraries:' "$repo/os/Containerfile"
wpe_copy_line=$(grep -n '^COPY --from=wpe-builder /wpe/ /$' "$repo/os/Containerfile" | tail -1 | cut -d: -f1)
wpe_deps_line=$(grep -n '&& gst-inspect-1.0 aacparse >/dev/null' "$repo/os/Containerfile" | tail -1 | cut -d: -f1)
shell_link_check_line=$(grep -n 'missing="$(ldd /usr/bin/shell' "$repo/os/Containerfile" | tail -1 | cut -d: -f1)
test "$wpe_copy_line" -lt "$wpe_deps_line"
test "$wpe_deps_line" -lt "$shell_link_check_line"
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
printf 'desktop %s\n' "$*" >>"$TILO_SESSION_TEST_LOG"
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
if [ "${TILO_SESSION_HOLD:-0}" = 1 ]; then
  trap 'printf "desktop-term\n" >>"$TILO_SESSION_TEST_LOG"; exit 0' TERM HUP INT
  while :; do sleep 1; done
fi
exit "${TILO_SESSION_DESKTOP_STATUS:-0}"
SH
chmod 0755 "$fake_systemctl" "$fake_dbus_update" "$fake_shell"
tilo_stage_script "$wrapper" "$test_wrapper" \
  SYSTEMCTL="$fake_systemctl" \
  DBUS_UPDATE="$fake_dbus_update" \
  SHELL_BIN="$fake_shell" \
  FLIGHT_RECORDER="$repo/dist/session/tilo-flight-recorder.py"
grep -Fqx "readonly SYSTEMCTL=$fake_systemctl" "$test_wrapper"
grep -Fqx "readonly SHELL_BIN=$fake_shell" "$test_wrapper"

# Successful verification starts the desktop, preserves its exit status, and
# tears down the Tilo target afterward.
export XDG_STATE_HOME="$tmp/state"
flight_dir="$XDG_STATE_HOME/tilo/session-logs"
set +e
XDG_RUNTIME_DIR="$runtime_dir" TILO_SESSION_TEST_LOG="$log" TILO_SESSION_DESKTOP_STATUS=7 \
  "$test_wrapper" --backend drm >"$tmp/stdout"
status=$?
set -e
[ "$status" = 7 ]
# The compositor's output still reaches the journal (the wrapper's stdout),
# and the flight record keeps it with a start and an end record.
grep -Fqx "desktop-output --backend drm trace=$flight_dir/frame-trace.csv" "$tmp/stdout"
head -n 1 "$flight_dir/latest.log" | grep -Eq '^tilo-flight-record-v1 start=.* pid=[0-9]+ shell='
grep -Fqx "desktop-output --backend drm trace=$flight_dir/frame-trace.csv" "$flight_dir/latest.log"
tail -n 1 "$flight_dir/latest.log" | grep -Eq '^tilo-flight-record-v1 end=.* status=7 logout_request=no$'
[ "$(stat -c %a "$flight_dir")" = 700 ]
[ "$(stat -c %a "$flight_dir/latest.log")" = 600 ]
[ "$(stat -c %a "$flight_dir/frame-trace.csv")" = 600 ]
[ -z "$(ls -A "$runtime_dir")" ] || { ls -la "$runtime_dir" >&2; exit 1; }
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
tail -n 1 "$flight_dir/latest.log" | grep -Eq 'status=0 logout_request=yes$'
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
