#!/bin/sh
#
# Run one command with fdwatch armed. See fdwatch.c for what it records
# and why.
#
#   scripts/fd-interpose/with-fdwatch.sh cargo run --bin shell -- --mode native
#
# For a whole take, do not wrap the rig — export the two variables in the
# shell that starts it instead, because the rig builds every session's
# environment from `os.environ` and so passes them down to each
# compositor, to `qos-launcher`, and to every sandboxed app:
#
#   export LD_PRELOAD=$PWD/scripts/fd-interpose/fdwatch.so
#   export QOS_FDWATCH=1 QOS_FDWATCH_LOG=$PWD/target/fdwatch-%p.log
#   scripts/yc-demo-live.py …
#
# Afterwards, the file with a `was-not-open` line in it is the process
# that closed a descriptor it did not own, and the `from` on that line
# names the object and offset. `addr2line -e <object> <offset>` finishes
# the job; the maps dump at the top of the file covers the case where
# dladdr could not.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
[ -f "$here/fdwatch.so" ] || "$here/build.sh"

export LD_PRELOAD="$here/fdwatch.so${LD_PRELOAD:+:$LD_PRELOAD}"
export QOS_FDWATCH=1
: "${QOS_FDWATCH_LOG:=}"
[ -n "$QOS_FDWATCH_LOG" ] && export QOS_FDWATCH_LOG

exec "$@"
