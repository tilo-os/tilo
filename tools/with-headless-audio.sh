#!/usr/bin/env bash
# Run tests against an isolated software audio endpoint, with no desktop/session.
# GStreamer still decodes and renders through its production PulseAudio sink.
# This proves the transport pipeline, not physical speaker output.
set -euo pipefail
if [ "$#" -eq 0 ]; then
    echo "usage: $0 COMMAND [ARG ...]" >&2
    exit 2
fi
command -v pulseaudio >/dev/null || {
    echo "headless audio requires pulseaudio (included in the CI builder)" >&2
    exit 1
}
# Pulse clients split PULSE_SERVER at whitespace, and Unix socket paths have a
# small length limit. Use a short private runtime path even if TMPDIR contains
# spaces or points at a deeply nested build directory.
audio_dir=$(mktemp -d /tmp/tilo-audio.XXXXXXXX)
audio_pid=
cleanup() {
    if [ -n "$audio_pid" ]; then
        # This foreground child has no PID file. Reap the process we started
        # before removing its runtime directory or completing the test run.
        kill "$audio_pid" 2>/dev/null || true
        wait "$audio_pid" 2>/dev/null || true
    fi
    rm -rf -- "$audio_dir"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
chmod 700 "$audio_dir"
export PULSE_RUNTIME_PATH="$audio_dir"
export PULSE_STATE_PATH="$audio_dir/state"
export PULSE_COOKIE="$audio_dir/cookie"
export PULSE_SERVER="unix:$audio_dir/native"
# Tests that prove output through the system sink run, and fail rather than
# skip, only where an endpoint is known to exist — which it is from here on.
export TILO_REQUIRE_AUDIO=1
# The socket is reachable only inside our mode-0700 temporary directory.
# Load the output before opening the socket so readiness includes the endpoint.
# Pulse parses module arguments itself after shell quoting has been removed.
socket_path=${audio_dir//\\/\\\\}/native
socket_path=${socket_path//\"/\\\"}
# Timestamped info-level logging shows how far a slow or stuck start got.
pulseaudio --daemonize=no --exit-idle-time=-1 --use-pid-file=no \
    --high-priority=no --realtime=no --disable-shm=yes --log-target=stderr -n \
    --log-level=info --log-time=yes \
    --load="module-null-sink sink_name=tilo_test_output" \
    --load="module-native-protocol-unix socket=\"$socket_path\" auth-anonymous=1" \
    >"$audio_dir/server.log" 2>&1 &
audio_pid=$!
# Commands can observe their actual owned endpoint without relying on the
# kernel's optional /proc/PID/task/PID/children interface.
export TILO_HEADLESS_AUDIO_PID="$audio_pid"
# The socket is bound and listening only once both modules have loaded, so it
# is the readiness signal. On a loaded runner (cold page cache, saturated
# disk) merely exec'ing the daemon and its libraries can take several
# seconds, so the cap is generous; a daemon that exits ends the wait at once.
timeout=${TILO_HEADLESS_AUDIO_TIMEOUT:-60}
started=$SECONDS
while [ ! -S "$audio_dir/native" ]; do
    if ! kill -0 "$audio_pid" 2>/dev/null; then
        if wait "$audio_pid"; then daemon_status=0; else daemon_status=$?; fi
        audio_pid=
        echo "headless audio: pulseaudio exited with status $daemon_status" \
            "after $((SECONDS - started))s without exposing its endpoint" >&2
        break
    fi
    if [ $((SECONDS - started)) -ge "$timeout" ]; then
        state=$(awk '{print $3}' "/proc/$audio_pid/stat" 2>/dev/null || true)
        wchan=$(cat "/proc/$audio_pid/wchan" 2>/dev/null || true)
        echo "headless audio: pulseaudio (pid $audio_pid, state ${state:-?}," \
            "wchan ${wchan:-?}) exposed no endpoint within ${timeout}s" >&2
        break
    fi
    sleep 0.1
done
if [ -S "$audio_dir/native" ]; then
    if "$@"; then status=0; else status=$?; fi
    if [ "$status" -ne 0 ]; then cat "$audio_dir/server.log" >&2; fi
    exit "$status"
fi
echo "headless audio: pulseaudio log:" >&2
cat "$audio_dir/server.log" >&2
df -h "$audio_dir" >&2 || true
echo "headless audio failed to expose its isolated endpoint" >&2
exit 1
