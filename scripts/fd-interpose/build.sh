#!/bin/sh
#
# Build fdwatch.so, then prove it works before saying it does.
#
# The check is not optional and not a separate script, because a
# diagnostic that is silently inert looks exactly like a clean run — and
# a clean run is what this is meant to contradict. It closes a descriptor
# twice on purpose and fails the build unless the library says so.
#
# Needs a host C compiler and nothing else: no qOS headers, no container.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
cc=${CC:-cc}
out="$here/fdwatch.so"

"$cc" -shared -fPIC -O2 -g -Wall -Wextra -Wno-unused-parameter \
      -o "$out" "$here/fdwatch.c" -ldl

check_log=$(mktemp)
trap 'rm -f "$check_log"' EXIT

# Three things at once: a close of a live AF_UNIX socket, a close of the
# same number afterwards (the shape this exists to catch), and a dup2
# onto a number that is free (the shape it must NOT cry wolf about —
# every service-fd handoff in the launcher looks like that, and calling
# ten of them double closes once sent a real investigation down a blind
# alley).
LD_PRELOAD="$out" QOS_FDWATCH=1 QOS_FDWATCH_LOG="$check_log" \
    python3 -c '
import os, socket
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
fd = os.dup(s.fileno())
os.close(fd)
try:
    os.close(fd)
except OSError:
    pass
spare = os.open("/dev/null", os.O_RDONLY)
free = 60
os.dup2(spare, free)          # onto a free number: routine, not a loss
os.close(free)
os.close(spare)
t = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
try:
    t.shutdown(socket.SHUT_RD)   # the one poll cannot see from the far side
except OSError:
    pass
t.close()
s.close()
'

fail() { echo "fdwatch built but $1 — not usable" >&2; exit 1; }

grep -q "DOUBLE-CLOSE" "$check_log" || fail "did not report a double close"
grep -q "socket domain=1" "$check_log" || fail "did not recognise an AF_UNIX socket"
if grep "dup2" "$check_log" | grep -q "DOUBLE-CLOSE"; then
    fail "called a dup2 onto a free descriptor a double close"
fi

grep -q "shutdown fd=" "$check_log" || fail "did not record a shutdown"

echo "built $out"
echo "checked: reports a double close, identifies AF_UNIX sockets, records"
echo "         shutdown, and does not mistake a dup2 onto a free descriptor"
echo "         for a loss"
