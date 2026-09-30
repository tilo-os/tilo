#!/usr/bin/env bash
# Spawn the tilo compositor headlessly + wait for the control socket.
#
# Usage:
#   ./scripts/headless-examples/spawn-headless.sh [SOCKET]
#
# SOCKET defaults to $XDG_RUNTIME_DIR/tilo-session/test-control.sock, the
# path tilo-control uses when nothing else is named. Never /tmp: another
# account could bind a shared name first and receive every command.
#
# Environment:
#   TILO_HEADLESS_TIMEOUT  seconds to wait for the socket (default 60; a
#                          cold debug build can take well over 15).
#   TILO_HEADLESS_PROFILE  directory to keep a scratch profile in across
#                          runs. Unset, each run gets a fresh one that is
#                          removed when the compositor exits.
#
# Behaviour:
#   - Runs against a scratch profile: HOME, the four XDG homes, the device
#     identity root (TILO_IDENTITY_ROOT) and the filesystem daemon's
#     handoff directories all point inside it, so the compositor never
#     reads or writes your real identity, settings or workspaces, nor
#     talks to the machine's filesystem daemon. A profile that resolves
#     to your real config is refused.
#   - Removes a stale socket at the chosen path, but refuses one a live
#     compositor still answers on — that is someone's session.
#   - Sets TILO_TEST_CONTROL to the socket path.
#   - Unsets DISPLAY + WAYLAND_DISPLAY so the binary boots truly headless.
#   - Runs ./target/debug/shell --backend headless.
#   - Prints "PROFILE <dir>" to stderr and "READY <socket>" to stdout once
#     usable; exits non-zero on timeout (the compositor's logs go to
#     stderr so callers can grep them).
#
# Lifetime:
#   The compositor stays alive until killed. Either run this in the
#   foreground (Ctrl+C tears it down) or in the background and `kill`
#   the PID:
#     ./scripts/headless-examples/spawn-headless.sh &
#     COMP=$!
#     trap "kill $COMP" EXIT

set -euo pipefail

SOCKET="${1:-${XDG_RUNTIME_DIR:?XDG_RUNTIME_DIR is unset; pass a socket path in a private directory}/tilo-session/test-control.sock}"
BINARY="$(dirname "$0")/../../target/debug/shell"
TIMEOUT="${TILO_HEADLESS_TIMEOUT:-60}"

if ! [[ "$TIMEOUT" =~ ^[1-9][0-9]*$ ]]; then
    echo "FAIL: TILO_HEADLESS_TIMEOUT must be a whole number of seconds, got '$TIMEOUT'" >&2
    exit 1
fi

if [ ! -x "$BINARY" ]; then
    echo "FAIL: compositor not built at $BINARY" >&2
    echo "      run: cargo build -p shell --bin shell" >&2
    exit 1
fi

REAL_CONFIG="$(realpath -m -- "${XDG_CONFIG_HOME:-$HOME/.config}")"
if [ -n "${TILO_HEADLESS_PROFILE:-}" ]; then
    PROFILE="$(realpath -m -- "$TILO_HEADLESS_PROFILE")"
    KEEP_PROFILE=1
else
    # On disk, not in XDG_RUNTIME_DIR: app caches outgrow a tmpfs.
    CACHE_ROOT="${XDG_CACHE_HOME:-$HOME/.cache}"
    mkdir -p -- "$CACHE_ROOT"
    PROFILE="$(mktemp -d -- "$CACHE_ROOT/tilo-headless.XXXXXX")"
    KEEP_PROFILE=0
fi
case "$REAL_CONFIG/" in
    "$PROFILE"/*)
        echo "FAIL: profile $PROFILE holds your real config ($REAL_CONFIG)" >&2
        exit 1
        ;;
esac
mkdir -p -m 700 -- "$PROFILE"/{config,data,state,cache,home,identity,system/run,system/fs-cache,system/peer-cache}
chmod 700 -- "$PROFILE"
REAL_HOME="$(realpath -m -- "$HOME")"
if [ "$(realpath -m -- "$PROFILE/home")" = "$REAL_HOME" ]; then
    echo "FAIL: profile $PROFILE/home is your real HOME ($REAL_HOME)" >&2
    exit 1
fi
echo "PROFILE $PROFILE" >&2

if [ -S "$SOCKET" ]; then
    if python3 - "$SOCKET" <<'EOF'; then
import socket, sys
s = socket.socket(socket.AF_UNIX)
s.settimeout(1)
try:
    s.connect(sys.argv[1])
except OSError:
    sys.exit(1)
EOF
        echo "FAIL: a live compositor answers on $SOCKET; pass another socket path" >&2
        [ "$KEEP_PROFILE" = 1 ] || rm -rf -- "$PROFILE"
        exit 1
    fi
    rm -f -- "$SOCKET"
fi
mkdir -p -m 700 -- "$(dirname -- "$SOCKET")"

COMP_PID=
cleanup() {
    if [ -n "$COMP_PID" ]; then
        kill "$COMP_PID" 2>/dev/null || true
        wait "$COMP_PID" 2>/dev/null || true
    fi
    [ ! -S "$SOCKET" ] || rm -f -- "$SOCKET"
    [ "$KEEP_PROFILE" = 1 ] || rm -rf -- "$PROFILE"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

TILO_TEST_CONTROL="$SOCKET" \
    HOME="$PROFILE/home" \
    XDG_CONFIG_HOME="$PROFILE/config" \
    XDG_DATA_HOME="$PROFILE/data" \
    XDG_STATE_HOME="$PROFILE/state" \
    XDG_CACHE_HOME="$PROFILE/cache" \
    TILO_IDENTITY_ROOT="$PROFILE/identity" \
    TILO_AUTHORITY_SOCKET_DIR="$PROFILE/system/run" \
    TILO_FS_AUTHORITY_CACHE_DIR="$PROFILE/system/fs-cache" \
    TILO_FS_CONTENT_PEER_CACHE_DIR="$PROFILE/system/peer-cache" \
    env -u DISPLAY -u WAYLAND_DISPLAY \
    "$BINARY" --backend headless &
COMP_PID=$!

deadline=$((SECONDS + TIMEOUT))
while [ "$SECONDS" -lt "$deadline" ]; do
    if [ -S "$SOCKET" ]; then
        echo "READY $SOCKET"
        status=0
        wait "$COMP_PID" || status=$?
        COMP_PID=
        exit "$status"
    fi
    if ! kill -0 "$COMP_PID" 2>/dev/null; then
        status=0
        wait "$COMP_PID" || status=$?
        COMP_PID=
        echo "FAIL: compositor exited ($status) before its socket appeared" >&2
        exit 1
    fi
    sleep 0.5
done

echo "FAIL: socket did not appear within ${TIMEOUT}s (raise TILO_HEADLESS_TIMEOUT)" >&2
exit 1
