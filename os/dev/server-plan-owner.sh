#!/usr/bin/env bash
# The owner device of the server plan install cycle, driven over SSH: the
# person's session a dev image starts with nobody at its screen
# (`dist/dev/nested-session.sh` mints its identity the same way), and the
# commands this cycle starts in it.
#
#   server-plan-owner.sh ROOT start         make the profile and start the session
#   server-plan-owner.sh ROOT wait          wait until the session answers
#   server-plan-owner.sh ROOT stop          stop the session and its bus
#   server-plan-owner.sh ROOT cmd CMD...    start a command in the profile
#   server-plan-owner.sh ROOT control ...   tilo-control against the session
#
# ROOT is a private directory on the guest; nothing of the person's real
# profile is read. `cmd` carries the same XDG and identity environment the
# session has, so `tilo` and the session are one device.
set -euo pipefail

root="${1:?usage: server-plan-owner.sh ROOT start|wait|stop|cmd|control ...}"
mode="${2:?usage: server-plan-owner.sh ROOT start|wait|stop|cmd|control ...}"
shift 2

guest_bin() { command -v "$1" 2>/dev/null || printf '/usr/libexec/tilo/%s\n' "$1"; }
session_bin="$(guest_bin tilo-session)"
control_bin="$(guest_bin tilo-control)"

profile_env() {
  export HOME="$root/home"
  export XDG_RUNTIME_DIR="$root/runtime"
  export XDG_CONFIG_HOME="$root/config"
  export XDG_DATA_HOME="$root/data"
  export XDG_STATE_HOME="$root/state"
  export XDG_CACHE_HOME="$root/cache"
  export TILO_STATE_PATH="$root/state.json"
  # The session keeps its identity under $XDG_CONFIG_HOME/tilo/identity, and
  # the CLI reads its device there: one identity for the profile, not two.
  export TILO_IDENTITY_DIR="$root/config/tilo/identity"
  export TILO_IDENTITY_ROOT="$root/config/tilo/identity"
  export TILO_WORKSPACE_ROOTS=""
  export TILO_TEST_CONTROL="$root/runtime/control.sock"
  export RUST_LOG="${RUST_LOG:-warn}"
  unset WAYLAND_DISPLAY DISPLAY
  if [ -r "$root/dbus.address" ]; then
    export DBUS_SESSION_BUS_ADDRESS="$(cat "$root/dbus.address")"
  fi
}

prepare() {
  local dir
  for dir in runtime config data state cache home logs; do
    mkdir -p "$root/$dir"
  done
  chmod 700 "$root/runtime"
}

start_bus() {
  [ -s "$root/dbus.address" ] && return 0
  # The daemon keeps the descriptor it wrote its address to, so a pipe would
  # wait for the daemon to end: write it to a file instead.
  if command -v dbus-daemon >/dev/null 2>&1 \
    && dbus-daemon --session --fork --print-address=1 --print-pid=1 \
      > "$root/dbus.out" 2>/dev/null; then
    sed -n '1p' "$root/dbus.out" > "$root/dbus.address"
    [ -s "$root/dbus.address" ] && return 0
  fi
  # The image's own bus is dbus-broker, which takes its listener from socket
  # activation rather than making one, and Fedora does not carry dbus-daemon
  # beside it. A broker of this profile's own keeps the session's names off
  # the desktop's bus, which another session on this machine already holds.
  if command -v dbus-broker-launch >/dev/null 2>&1 \
    && command -v systemd-socket-activate >/dev/null 2>&1; then
    local runtime="$root/runtime" attempt
    rm -f "$runtime/bus"
    XDG_RUNTIME_DIR="$runtime" setsid \
      systemd-socket-activate -l "$runtime/bus" dbus-broker-launch --scope user \
      > "$root/logs/bus.log" 2>&1 &
    for attempt in $(seq 1 300); do
      [ -S "$runtime/bus" ] && break
      sleep 0.1
    done
    if [ -S "$runtime/bus" ]; then
      printf 'unix:path=%s/bus\n' "$runtime" > "$root/dbus.address"
      return 0
    fi
  fi
  # Nothing to start is not a reason to stop: the session answers its
  # questions over its own control socket, and says in its log what a bus it
  # could not have cost it.
  echo "the owner profile starts without a session bus" >&2
  return 0
}

# The throwaway identity a development profile starts with: minted by a
# preparatory session, then read by the session the cycle drives.
mint_identity() {
  [ -f "$TILO_IDENTITY_DIR/throwaway-test-identity" ] && return 0
  rm -f "$root/runtime/control.sock"
  "$session_bin" --backend headless --size 1280x800 \
    > "$root/logs/identity.log" 2>&1 &
  local pid=$!
  local attempt
  for attempt in $(seq 1 1200); do
    [ -S "$root/runtime/control.sock" ] && break
    kill -0 "$pid" 2>/dev/null || break
    sleep 0.1
  done
  [ -S "$root/runtime/control.sock" ] || {
    tail -n 40 "$root/logs/identity.log" >&2 || true
    kill "$pid" 2>/dev/null || true
    return 1
  }
  local answer
  answer="$("$control_bin" --socket "$root/runtime/control.sock" \
    raw '{"op":"provision_test_identity"}')" || true
  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
  rm -f "$root/runtime/control.sock"
  printf '%s\n' "$answer" | grep -q '"throwaway": *true' \
    || printf '%s\n' "$answer" | grep -q '"code": *"identity_exists"' || {
    printf 'the owner profile minted no identity: %s\n' "$answer" >&2
    return 1
  }
}

case "$mode" in
  start)
    prepare
    start_bus
    # The session, its control socket and its identity all live in the
    # profile, so they start with the profile's environment, bus included.
    profile_env
    if [ -s "$root/session.pid" ] && kill -0 "$(<"$root/session.pid")" 2>/dev/null; then
      exit 0
    fi
    mint_identity
    : > "$root/logs/session.log"
    setsid "$session_bin" --backend headless --size 1280x800 \
      >> "$root/logs/session.log" 2>&1 &
    printf '%s\n' "$!" > "$root/session.pid"
    ;;
  wait)
    profile_env
    for _ in $(seq 1 1200); do
      if [ -S "$TILO_TEST_CONTROL" ] \
        && "$control_bin" --socket "$TILO_TEST_CONTROL" state-dump >/dev/null 2>&1; then
        exit 0
      fi
      sleep 0.5
    done
    tail -n 80 "$root/logs/session.log" >&2 || true
    echo "the owner session did not answer" >&2
    exit 1
    ;;
  stop)
    if [ -s "$root/session.pid" ]; then
      kill -- "-$(cat "$root/session.pid")" 2>/dev/null || true
      rm -f "$root/session.pid"
    fi
    ;;
  control)
    profile_env
    exec "$control_bin" --socket "$TILO_TEST_CONTROL" "$@"
    ;;
  cmd)
    profile_env
    exec "$@"
    ;;
  *)
    echo "unknown mode: $mode" >&2
    exit 2
    ;;
esac
