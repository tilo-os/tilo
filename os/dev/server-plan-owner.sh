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
#   server-plan-owner.sh ROOT diagnose      every thread's stack of the daemon and
#                                           the session, into the profile's logs
#
# ROOT is a private directory on the guest; nothing of the person's real
# profile is read. `cmd` carries the same XDG and identity environment the
# session has, so `tilo` and the session are one device. The profile's device
# daemon writes its own log to ROOT/logs/device-daemon.log, which the cycle
# copies as evidence.
set -euo pipefail

root="${1:?usage: server-plan-owner.sh ROOT start|wait|stop|cmd ...}"
mode="${2:?usage: server-plan-owner.sh ROOT start|wait|stop|cmd ...}"
shift 2

guest_bin() { command -v "$1" 2>/dev/null || printf '/usr/libexec/tilo/%s\n' "$1"; }
session_bin="$(guest_bin tilo-session)"
control_bin="$(guest_bin tilo-control)"
tilo_bin="$(guest_bin tilo)"

profile_env() {
  export HOME="$root/home"
  export XDG_RUNTIME_DIR="$root/runtime"
  export XDG_CONFIG_HOME="$root/config"
  export XDG_DATA_HOME="$root/data"
  export XDG_STATE_HOME="$root/state"
  export XDG_CACHE_HOME="$root/cache"
  export TILO_STATE_PATH="$root/state.json"
  # The session and the CLI keep the person under $XDG_CONFIG_HOME/tilo/identity
  # and this device's key where TILO_IDENTITY_ROOT names: one identity for the
  # profile, not two.
  export TILO_IDENTITY_ROOT="$root/config/tilo/identity"
  # The profile's own device daemon serves it by name. Unnamed, a daemon on Tilo
  # OS is the system unit's, whose profile and sockets belong to the system's
  # account, and this one, run as the guest's user, could keep neither. Named,
  # its sockets are a private directory under $XDG_RUNTIME_DIR/tilo, and the
  # CLI and the session started with this environment ask it there.
  export TILO_RUNTIME_PROFILE="$root/daemon"
  # The profile's own bus answers no Secret Service, so its secrets (the
  # channels' group keys a sealed write needs among them) go to the sealed
  # file store a headless machine uses, as dist/dev/nested-session.sh and the
  # hosted guest's headless session name it.
  export TILO_SECRETS_DIR="$root/secrets"
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
  for dir in runtime config data state cache home logs secrets daemon; do
    mkdir -p "$root/$dir"
  done
  chmod 700 "$root/runtime" "$root/secrets" "$root/daemon"
}

# The profile's device daemon, started once and kept for the profile's life: it
# makes the person, and the session and every command the cycle runs in the
# profile are its callers. Its own log is ROOT/logs/device-daemon.log, named
# apart from the session's, so what it refused is read on its own when a run
# fails; it says what it decided there at info, below the rest of the profile's
# warnings. Bounded: a daemon that never answers answers the cycle's question
# nowhere.
start_daemon() {
  if [ -s "$root/daemon.pid" ] && kill -0 "$(<"$root/daemon.pid")" 2>/dev/null; then
    return 0
  fi
  printf -- '--- the device daemon started\n' >> "$root/logs/device-daemon.log"
  # Debug from the first line unless the cycle names another level: a step that stalls says
  # what it was doing when it stopped answering. The transports' own chatter stays at info.
  env RUST_LOG="${TILO_SERVER_CYCLE_DAEMON_LOG:-debug,iroh=info,iroh_relay=info,quinn=info,quinn_proto=info,netlink_proto=info,hickory_proto=info,hickory_resolver=info,swarm_discovery=info,h2=info,hyper=info,hyper_util=info,rustls=info,wgpu_core=warn,wgpu_hal=warn,naga=warn,smithay=info,calloop=info}" \
    setsid "$(guest_bin tilo-runtime)" >> "$root/logs/device-daemon.log" 2>&1 < /dev/null &
  printf '%s\n' "$!" > "$root/daemon.pid"
  # The daemon names its endpoint in its profile as a link, whatever directory a socket's path
  # fits in: the profile is where the host that starts it over one finds it, so this asks no
  # guess at the sockets' own layout.
  local socket=$root/daemon/native.sock
  for _ in $(seq 1 360); do
    [ -S "$socket" ] && return 0
    kill -0 "$(<"$root/daemon.pid")" 2>/dev/null || break
    sleep 0.5
  done
  tail -n 40 "$root/logs/device-daemon.log" >&2 || true
  echo "the owner profile's daemon did not answer" >&2
  return 1
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

# The identity a disposable profile has: one the CLI made, so its keys and
# its files sit in the tree the CLI and the session both read.
mint_identity() {
  [ -f "$TILO_IDENTITY_ROOT/device-private/keypair.bin" ] && return 0
  # A person is made by the device's daemon alone. Bounded: a login that waits
  # on something answers the cycle's question nowhere.
  timeout 180 "$tilo_bin" login --new-identity --name "plan cycle owner" \
    > "$root/logs/identity.log" 2>&1 || {
    tail -n 40 "$root/logs/identity.log" "$root/logs/device-daemon.log" >&2 || true
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
    start_daemon
    mint_identity
    : > "$root/logs/session.log"
    setsid "$session_bin" --backend headless --size 1280x800 \
      >> "$root/logs/session.log" 2>&1 &
    printf '%s\n' "$!" > "$root/session.pid"
    date +%s > "$root/session.started"
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
    tail -n 80 "$root/logs/session.log" "$root/logs/device-daemon.log" >&2 || true
    echo "the owner session did not answer" >&2
    exit 1
    ;;
  stop)
    if [ -s "$root/session.pid" ]; then
      kill -- "-$(cat "$root/session.pid")" 2>/dev/null || true
      rm -f "$root/session.pid"
    fi
    if [ -s "$root/daemon.pid" ]; then
      kill -- "-$(cat "$root/daemon.pid")" 2>/dev/null || true
      rm -f "$root/daemon.pid"
    fi
    ;;
  cmd)
    profile_env
    exec "$@"
    ;;
  diagnose)
    # What the daemon and the session were doing once the session's first
    # asks had 45 seconds to be answered: every thread's stack, by whichever
    # debugger the image carries, and what the kernel says of each thread
    # otherwise.
    if [ -s "$root/session.started" ]; then
      waited=$(( $(date +%s) - $(<"$root/session.started") ))
      [ "$waited" -ge 45 ] || sleep $(( 45 - waited ))
    fi
    for name in daemon session; do
      out="$root/logs/stacks-$name.txt"
      pid="$(cat "$root/$name.pid" 2>/dev/null || true)"
      if [ -z "$pid" ] || ! kill -0 "$pid" 2>/dev/null; then
        echo "the $name is not running" > "$out"
        continue
      fi
      {
        echo "# $name, pid $pid, $(date -u +%FT%TZ), $(( $(date +%s) - $(cat "$root/session.started" 2>/dev/null || date +%s) )) s after the session started"
        if command -v gdb >/dev/null 2>&1; then
          timeout 120 gdb -p "$pid" -batch -nx -ex 'set pagination off' -ex 'thread apply all bt' 2>&1
        elif command -v eu-stack >/dev/null 2>&1; then
          timeout 120 eu-stack -p "$pid" 2>&1
        else
          echo "# no gdb or eu-stack on this image: each thread's name, wait channel, syscall and kernel stack"
          for task in /proc/"$pid"/task/*; do
            echo "## thread $(basename "$task") $(cat "$task/comm" 2>/dev/null) wchan=$(cat "$task/wchan" 2>/dev/null) state=$(awk '{print $3}' "$task/stat" 2>/dev/null)"
            cat "$task/syscall" 2>/dev/null
            cat "$task/stack" 2>/dev/null
          done
        fi
      } > "$out" 2>&1
    done
    ;;
  *)
    echo "unknown mode: $mode" >&2
    exit 2
    ;;
esac
