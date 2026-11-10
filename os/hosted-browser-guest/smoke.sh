#!/bin/sh
# Prove, inside the image build, that this root filesystem starts what a boot
# would: the Chromium engine host answers its `ready` event, and the guest's
# headless session comes up on Mesa's software driver and serves its display.
# The build fails here rather than on a host's first environment.
#
# It runs as the tenant (uid 1000): Chromium refuses root, and the agent starts
# every program of the environment as that user, so root would prove nothing.
set -eu

version=${CHROMIUM_RUNTIME_VERSION:?the engine runtime's version is not set}
engine="/usr/lib/tilo-runtime/$version/lib/chromium/tilo-chromium-host"
test -x "$engine"
# The session runs on its own session bus, which this root filesystem serves
# itself: there is no systemd here to have started one.
command -v dbus-run-session >/dev/null
command -v setpriv >/dev/null

work=$(mktemp -d)
trace() {
    status=$?
    if [ "$status" != 0 ]; then
        printf -- '--- engine ---\n' >&2
        sed -e 's/^/  /' "$work/engine.log" >&2 2>/dev/null || true
        printf -- '--- session ---\n' >&2
        sed -e 's/^/  /' "$work/session.log" >&2 2>/dev/null || true
        printf -- '--- identity ---\n' >&2
        sed -e 's/^/  /' "$work/identity.log" >&2 2>/dev/null || true
    fi
    rm -rf "$work"
    return "$status"
}
trap trace EXIT

# The tenant's own files are its own: the engine writes its profile, and the
# session its runtime directory.
chown 1000:1000 "$work"
as_tilo() {
    setpriv --reuid=1000 --regid=1000 --init-groups env HOME=/home/tilo "$@"
}

# Wait for FILE to hold TEXT, or stop waiting: never longer than the bound.
wait_for_text() {
    text=$1 file=$2 bound=$3 what=$4
    tries=$((bound * 5))
    while [ "$tries" -gt 0 ]; do
        if grep -qF -- "$text" "$file" 2>/dev/null; then
            return 0
        fi
        tries=$((tries - 1))
        sleep 0.2
    done
    echo "the browser guest smoke: $what never came" >&2
    return 1
}

# ---- The engine ---------------------------------------------------------------
# The host reads framed commands from its standard input and writes framed
# events to its standard output; it stops when its input ends. The input stays
# open until the ready event has been read.
mkfifo "$work/commands"
chown 1000:1000 "$work/commands"
as_tilo setsid "$engine" --profile="$work/profile" \
    < "$work/commands" > "$work/engine.out" 2> "$work/engine.log" &
engine_pid=$!
exec 3> "$work/commands"
wait_for_text '"event":"ready"' "$work/engine.out" 120 'the Chromium engine ready event'
echo 'engine: the Chromium engine host answers ready'
kill -TERM -"$engine_pid" 2>/dev/null || kill -TERM "$engine_pid" 2>/dev/null || true
exec 3>&-
wait "$engine_pid" 2>/dev/null || true

# ---- The session --------------------------------------------------------------
# Through the script the guest agent runs, so the smoke proves that path too: a
# device mints its own identity on first start, and this one has to, because
# joining the workspace is what the browser guest exists for.
printf 'TILO_SESSION_ARGS=\n' > "$work/session.env"
chown 1000:1000 "$work/session.env"
as_tilo setsid /usr/libexec/tilo/browser-session "$work/session.env" \
    > "$work/session.log" 2>&1 &
session_pid=$!
runtime_dir=/home/tilo/.local/share/tilo/runtime
tries=600
display=
while [ "$tries" -gt 0 ] && [ -z "$display" ]; do
    if ! kill -0 "$session_pid" 2>/dev/null; then
        echo 'the browser guest smoke: the session stopped before it served a display' >&2
        exit 1
    fi
    display=$(find "$runtime_dir" -maxdepth 1 -name 'wayland-*' -print -quit 2>/dev/null)
    tries=$((tries - 1))
    [ -n "$display" ] || sleep 0.2
done
if [ -z "$display" ]; then
    echo 'the browser guest smoke: the session never served a display' >&2
    exit 1
fi
printf 'session: the headless session serves %s\n' "$display"
kill -TERM -"$session_pid" 2>/dev/null || true
wait "$session_pid" 2>/dev/null || true

# The device the session made of this guest verifies the way a release gate
# checks an installed machine.
as_tilo /usr/bin/tilo-session --verify-collaboration-identity > "$work/identity.log" 2>&1
grep -q 'identity verified' "$work/identity.log"
echo 'identity: this guest is a device of its own'
