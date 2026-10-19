#!/usr/bin/env bash
# Run a command on its own private D-Bus session bus, and stop the bus when
# the command exits: what dbus-run-session does, for hosts that have
# dbus-daemon but not dbus-run-session (Tilo OS ships dbus-broker for the
# user's own bus and no dbus-run-session).
#
# Usage: dist/dev/session-bus.sh DBUS_DAEMON COMMAND [ARGS...]
#
# The bus socket lives in a fresh directory under $XDG_RUNTIME_DIR. The
# daemon reads the standard session configuration, so services are
# activated from the usual service directories, $XDG_DATA_HOME included.
set -euo pipefail

daemon=${1:?usage: session-bus.sh DBUS_DAEMON COMMAND [ARGS...]}
shift
(($#)) || { echo "session-bus: no command" >&2; exit 2; }

dir=$(mktemp -d "${XDG_RUNTIME_DIR:?XDG_RUNTIME_DIR is unset}/session-bus.XXXXXX")
socket=$dir/bus
"$daemon" --session --nofork --nopidfile --address="unix:path=$socket" &
bus_pid=$!
child_pid=
stop() {
    [[ -n $child_pid ]] && kill -TERM "$child_pid" 2>/dev/null
    kill -TERM "$bus_pid" 2>/dev/null || true
    wait "$bus_pid" 2>/dev/null || true
    rm -rf "$dir"
}
trap stop EXIT
trap 'exit 130' INT TERM HUP

tries=100
until [[ -S $socket ]]; do
    if ! kill -0 "$bus_pid" 2>/dev/null || ! ((tries--)); then
        echo "session-bus: $daemon did not start a bus at $socket" >&2
        exit 1
    fi
    sleep 0.05
done

export DBUS_SESSION_BUS_ADDRESS=unix:path=$socket
"$@" &
child_pid=$!
status=0
wait "$child_pid" || status=$?
child_pid=
exit "$status"
