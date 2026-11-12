#!/bin/sh
# Prove, inside the image build, that this root filesystem starts what a boot
# would: the Chromium engine host answers its `ready` event, and the guest's
# headless session comes up on Mesa's software driver and serves its display.
# The build fails here rather than on a host's first environment.
#
# It runs as the session's own user (tilo-browser): Chromium refuses root, and
# the session never runs as anything else, so root would prove nothing.
set -eu

version=${CHROMIUM_RUNTIME_VERSION:?CHROMIUM_RUNTIME_VERSION is not set}
engine="/usr/lib/tilo-runtime/$version/lib/chromium/tilo-chromium-host"
missing() {
    echo "the browser smoke: $1" >&2
    exit 1
}
[ -x "$engine" ] || missing "no Chromium engine host at $engine"
# The session runs on its own session bus, which this root filesystem serves
# itself: there is no systemd here to have started one.
command -v dbus-run-session >/dev/null || missing 'no dbus-run-session'
command -v setpriv >/dev/null || missing 'no setpriv'

work=$(mktemp -d)
trace() {
    status=$?
    if [ "$status" != 0 ]; then
        printf -- '--- engine ---\n' >&2
        sed -e 's/^/  /' "$work/engine.log" >&2 2>/dev/null || true
        printf -- '--- session ---\n' >&2
        sed -e 's/^/  /' "$work/session.log" >&2 2>/dev/null || true
    fi
    rm -rf "$work"
    return "$status"
}
trap trace EXIT

# The session user's own files are its own: the engine writes its profile, and
# the session its runtime directory.
chown tilo-browser:tilo-browser "$work"
as_browser_user() {
    setpriv --reuid=tilo-browser --regid=tilo-browser --init-groups env HOME=/home/tilo-browser "$@"
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
    echo "the browser smoke: $what never came" >&2
    return 1
}

# ---- The engine ---------------------------------------------------------------
# The host reads framed commands from its standard input and writes framed
# events to its standard output; it stops when its input ends. The input stays
# open until the ready event has been read.
mkfifo "$work/commands"
chown tilo-browser:tilo-browser "$work/commands"
as_browser_user setsid "$engine" --profile="$work/profile" \
    < "$work/commands" > "$work/engine.out" 2> "$work/engine.log" &
engine_pid=$!
exec 3> "$work/commands"
wait_for_text '"event":"ready"' "$work/engine.out" 120 'the Chromium engine ready event'
echo 'engine: the Chromium engine host answers ready'
kill -TERM -"$engine_pid" 2>/dev/null || kill -TERM "$engine_pid" 2>/dev/null || true
exec 3>&-
wait "$engine_pid" 2>/dev/null || true

# ---- The session --------------------------------------------------------------
# Through the script the guest agent runs, so the smoke proves that path too.
# Its identity is not proved here: a session is handed one by the relay at
# every start, and the image holds none.
printf 'TILO_SESSION_ARGS=\n' > "$work/session.env"
chown tilo-browser:tilo-browser "$work/session.env"
# Started directly, not through a function, so $! is the script itself, which
# a VM host signals.
setpriv --reuid=tilo-browser --regid=tilo-browser --init-groups env HOME=/home/tilo-browser \
    /usr/libexec/tilo/browser-session "$work/session.env" > "$work/session.log" 2>&1 &
session_pid=$!
runtime_dir=/home/tilo-browser/.local/share/tilo/runtime
tries=600
display=
while [ "$tries" -gt 0 ] && [ -z "$display" ]; do
    if ! kill -0 "$session_pid" 2>/dev/null; then
        echo 'the browser smoke: the session stopped before it served a display' >&2
        exit 1
    fi
    # Until the session made its runtime directory, find has none to read.
    display=$(find "$runtime_dir" -maxdepth 1 -name 'wayland-*' ! -name '*.lock' -print -quit 2>/dev/null || true)
    tries=$((tries - 1))
    [ -n "$display" ] || sleep 0.2
done
if [ -z "$display" ]; then
    echo 'the browser smoke: the session never served a display' >&2
    exit 1
fi
printf 'session: the headless session serves %s\n' "$display"
# SIGTERM ends it, and in time: the profile is saved after the session ended,
# and a VM host kills what has not ended within its grace.
kill -TERM "$session_pid" 2>/dev/null || true
tries=50
while [ "$tries" -gt 0 ] && kill -0 "$session_pid" 2>/dev/null; do
    tries=$((tries - 1))
    sleep 0.2
done
if kill -0 "$session_pid" 2>/dev/null; then
    echo 'the browser smoke: the session did not end within 10 s of SIGTERM' >&2
    ps -eo pid,pgid,stat,args >&2 || true
    exit 1
fi
wait "$session_pid" 2>/dev/null || true
echo 'session: ends on SIGTERM'

# ---- The Linux apps session ---------------------------------------------------
# The same session, as the tenant, through the script the guest agent runs for a
# workspace's Linux apps: it serves a display, and a Wayland program of this
# root filesystem connects to it and draws. Nothing it made is kept.
apps_home=/home/tilo
apps_runtime=$work/apps-runtime
install -d -m 0700 -o tilo -g tilo "$apps_runtime"
printf 'TILO_SESSION_ARGS=\nTILO_SESSION_RUNTIME_DIR=%s\n' "$apps_runtime" > "$work/apps.env"
chown tilo:tilo "$work/apps.env"
setpriv --reuid=tilo --regid=tilo --init-groups env HOME="$apps_home" \
    /usr/libexec/tilo/apps-session "$work/apps.env" > "$work/session.log" 2>&1 &
apps_pid=$!
tries=600
display=
while [ "$tries" -gt 0 ] && [ -z "$display" ]; do
    if ! kill -0 "$apps_pid" 2>/dev/null; then
        echo 'the browser smoke: the apps session stopped before it served a display' >&2
        exit 1
    fi
    display=$(find "$apps_runtime" -maxdepth 1 -name 'wayland-*' ! -name '*.lock' -print -quit 2>/dev/null || true)
    tries=$((tries - 1))
    [ -n "$display" ] || sleep 0.2
done
[ -n "$display" ] || { echo 'the browser smoke: the apps session never served a display' >&2; exit 1; }
printf 'apps: the tenant'"'"'s session serves %s\n' "$display"
if ! setpriv --reuid=tilo --regid=tilo --init-groups env HOME="$apps_home" \
    XDG_RUNTIME_DIR="$apps_runtime" WAYLAND_DISPLAY="${display##*/}" \
    timeout 60 foot --override=shell=/bin/true > "$work/foot.log" 2>&1; then
    sed -e 's/^/  /' "$work/foot.log" >&2
    echo 'the browser smoke: a Wayland program did not run in the apps session' >&2
    exit 1
fi
echo 'apps: a Wayland program ran in it'
kill -TERM "$apps_pid" 2>/dev/null || true
tries=50
while [ "$tries" -gt 0 ] && kill -0 "$apps_pid" 2>/dev/null; do
    tries=$((tries - 1))
    sleep 0.2
done
kill -0 "$apps_pid" 2>/dev/null && { echo 'the browser smoke: the apps session did not end within 10 s of SIGTERM' >&2; exit 1; }
wait "$apps_pid" 2>/dev/null || true
# The tenant's home keeps nothing of the session: no identity is in the image.
rm -rf "$apps_home/.config/tilo" "$apps_home/.local/share/tilo" "$apps_home/.local/state/tilo" \
    "$apps_home/.cache/tilo"
echo 'apps: ends on SIGTERM'
