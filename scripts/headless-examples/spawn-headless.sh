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
# Behaviour:
#   - Removes any stale socket at the chosen path (only a socket).
#   - Sets TILO_TEST_CONTROL to the socket path.
#   - Unsets DISPLAY + WAYLAND_DISPLAY so the binary boots truly headless.
#   - Runs ./target/debug/shell --backend headless.
#   - Waits up to 15s for the socket to appear.
#   - Prints "READY <socket>" to stdout once usable; exits non-zero on
#     timeout (the compositor's logs go to stderr so callers can grep
#     them).
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

if [ ! -x "$BINARY" ]; then
    echo "FAIL: compositor not built at $BINARY" >&2
    echo "      run: cargo build -p shell --bin shell" >&2
    exit 1
fi

if [ -S "$SOCKET" ]; then
    rm -f -- "$SOCKET"
fi

export TILO_TEST_CONTROL="$SOCKET"
exec env -u DISPLAY -u WAYLAND_DISPLAY \
    "$BINARY" --backend headless &
COMP_PID=$!

# Wait for the socket — up to 15s.
for _ in $(seq 1 30); do
    if [ -S "$SOCKET" ]; then
        echo "READY $SOCKET"
        wait $COMP_PID
        exit $?
    fi
    sleep 0.5
done

echo "FAIL: socket did not appear within 15s" >&2
kill $COMP_PID 2>/dev/null || true
exit 1
