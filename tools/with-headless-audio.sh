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
# The socket is reachable only inside our mode-0700 temporary directory.
# Load the output before opening the socket so readiness includes the endpoint.
# Pulse parses module arguments itself after shell quoting has been removed.
socket_path=${audio_dir//\\/\\\\}/native
socket_path=${socket_path//\"/\\\"}
pulseaudio --daemonize=no --exit-idle-time=-1 --use-pid-file=no \
    --high-priority=no --realtime=no --disable-shm=yes --log-target=stderr -n \
    --load="module-null-sink sink_name=tilo_test_output" \
    --load="module-native-protocol-unix socket=\"$socket_path\" auth-anonymous=1" \
    >"$audio_dir/server.log" 2>&1 &
audio_pid=$!
for ((attempt = 0; attempt < 100; attempt++)); do
    if [ -S "$audio_dir/native" ]; then
        if "$@"; then status=0; else status=$?; fi
        if [ "$status" -ne 0 ]; then cat "$audio_dir/server.log" >&2; fi
        exit "$status"
    fi
    if ! kill -0 "$audio_pid" 2>/dev/null; then break; fi
    sleep 0.05
done
cat "$audio_dir/server.log" >&2
echo "headless audio failed to expose its isolated endpoint" >&2
exit 1
