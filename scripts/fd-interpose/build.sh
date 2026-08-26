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

# One close of a live descriptor and one of the same number afterwards.
# The second is the shape this whole thing exists to catch.
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
s.close()
'

if ! grep -q "was-not-open" "$check_log"; then
    echo "fdwatch built but did not report a double close — not usable" >&2
    echo "--- what it did report ---" >&2
    grep -v '^fdwatch --- \|^[0-9a-f]*-[0-9a-f]* ' "$check_log" >&2 || true
    exit 1
fi
if ! grep -q "socket domain=1" "$check_log"; then
    echo "fdwatch built but did not recognise an AF_UNIX socket — not usable" >&2
    exit 1
fi

echo "built $out"
echo "checked: it reports a double close and identifies AF_UNIX sockets"
