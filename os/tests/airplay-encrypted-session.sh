#!/usr/bin/env bash
# AirPlay encrypted-session acceptance: PIN pairing, FairPlay setup, encrypted
# H.264 media and graceful teardown against the source-pinned receiver.
#
# Pairing is not media. Each lifecycle phase has its own deadline so a stall
# names the phase that died. Plugin discovery is warmed separately before the
# sender starts. This script is installed into the image and run against the
# assembled system; it is not an installation prerequisite.
set -euo pipefail

plugin_deadline_sec="${AIRPLAY_PLUGIN_DEADLINE_SEC:-30}"
codec_deadline_sec="${AIRPLAY_CODEC_DEADLINE_SEC:-30}"
setup_deadline_sec="${AIRPLAY_SETUP_DEADLINE_SEC:-30}"
media_deadline_sec="${AIRPLAY_MEDIA_DEADLINE_SEC:-60}"
teardown_deadline_sec="${AIRPLAY_TEARDOWN_DEADLINE_SEC:-10}"
force_cleanup_deadline_sec="${AIRPLAY_FORCE_CLEANUP_DEADLINE_SEC:-5}"

gst_inspect="${TILO_AIRPLAY_GST_INSPECT:-gst-inspect-1.0}"
gst_launch="${TILO_AIRPLAY_GST_LAUNCH:-gst-launch-1.0}"
receiver_bin="${TILO_DOUBLETAKE_TEST_RECEIVER:-/usr/libexec/tilo-doubletake-test-receiver}"
sender_bin="${TILO_DOUBLETAKE:-/usr/libexec/tilo-doubletake}"

test_root="$(mktemp -d)"
receiver_pid=""
sender_pid=""
receiver_log=""
sender_log=""

cleanup() {
  status=$?
  trap - EXIT
  if [[ -n "${sender_pid}" ]]; then
    kill -TERM "$sender_pid" 2>/dev/null || true
    kill -KILL "$sender_pid" 2>/dev/null || true
    wait "$sender_pid" 2>/dev/null || true
  fi
  if [[ -n "${receiver_pid}" ]]; then
    kill -TERM "$receiver_pid" 2>/dev/null || true
    wait "$receiver_pid" 2>/dev/null || true
  fi
  if (( status != 0 )); then
    echo "airplay encrypted-session test failed (status $status)" >&2
    for log in "$test_root/codec.log" "$receiver_log" "$sender_log"; do
      if [[ -n "$log" && -s "$log" ]]; then
        echo "--- $(basename "$log") ---" >&2
        sed 's/^/  /' "$log" >&2
      fi
    done
  fi
  rm -rf -- "$test_root"
  exit "$status"
}
trap cleanup EXIT

receiver_log="$test_root/receiver.log"
sender_log="$test_root/sender.log"

fail_phase() {
  echo "airplay $1 deadline exceeded ($2s)" >&2
  exit 1
}

# Wall clock in microseconds. $SECONDS only ticks at whole
# seconds, so a deadline measured with it could end almost at once.
now_us() {
  local now="${EPOCHREALTIME/[.,]/}"
  echo "$((10#$now))"
}

# Wait until CHECK succeeds within DEADLINE_SEC. CHECK always runs at least
# once. When REQUIRE_SENDER is 1, a sender exit before progress fails the
# named phase immediately.
wait_phase() {
  local phase="$1"
  local deadline_sec="$2"
  local check="$3"
  local require_sender="${4:-1}"
  local deadline=$(( $(now_us) + deadline_sec * 1000000 ))
  while true; do
    if "$check"; then
      return 0
    fi
    if (( $(now_us) >= deadline )); then
      break
    fi
    if [[ "$require_sender" == 1 && -n "${sender_pid}" ]] \
      && ! kill -0 "$sender_pid" 2>/dev/null; then
      echo "airplay ${phase} failed: sender exited before ${phase} progress" >&2
      exit 1
    fi
    sleep 0.05
  done
  fail_phase "$phase" "$deadline_sec"
}

setup_progress() {
  grep -Eq 'pairing=[1-9][0-9]*/[1-9][0-9]*' "$receiver_log" \
    && grep -Eq 'fairplay=[1-9][0-9]*' "$receiver_log" \
    && grep -Eq 'setup=[1-9][0-9]*' "$receiver_log"
}

media_progress() {
  grep -Eq 'video=[1-9][0-9]*/[1-9][0-9]*B' "$receiver_log"
}

teardown_progress() {
  grep -Eq 'teardown=[1-9][0-9]*' "$receiver_log"
}

receiver_listening() {
  grep -q "listening on 127.0.0.1:17000" "$receiver_log"
}

# Warm plugin discovery on its own budget. A hang here must not consume the
# media deadline; a missing encoder must fail closed before the session starts.
set +e
timeout --signal=TERM "${plugin_deadline_sec}s" \
  "$gst_inspect" openh264enc >/dev/null 2>&1
plugin_status=$?
set -e
if [[ "$plugin_status" == 124 || "$plugin_status" == 143 ]]; then
  fail_phase plugin-discovery "$plugin_deadline_sec"
fi
if (( plugin_status != 0 )); then
  echo "airplay plugin-discovery failed: openh264enc is not available" >&2
  exit 1
fi

# Factory discovery also succeeds against Fedora's noopenh264 compatibility
# stub. Exercise both real constructors and decoded frames before pairing.
set +e
timeout --signal=TERM --kill-after=5s "${codec_deadline_sec}s" \
  "$gst_launch" -q videotestsrc num-buffers=30 pattern=black ! \
  video/x-raw,format=I420,width=160,height=96,framerate=30/1 ! \
  openh264enc ! h264parse ! openh264dec ! video/x-raw,format=I420 ! \
  filesink "location=$test_root/decoded.i420" >"$test_root/codec.log" 2>&1
codec_status=$?
set -e
if [[ "$codec_status" == 124 || "$codec_status" == 137 || "$codec_status" == 143 ]]; then
  fail_phase codec-initialization "$codec_deadline_sec"
fi
if (( codec_status != 0 )) || [[ ! -f "$test_root/decoded.i420" ]] \
    || [[ $(stat -c %s "$test_root/decoded.i420") != 691200 ]]; then
  echo "airplay codec-initialization failed: OpenH264 must encode and decode 30 synthetic frames; plugin discovery alone is insufficient" >&2
  exit 1
fi

"$receiver_bin" \
  -listen 127.0.0.1:17000 -profile modern -auth pin -code 4821 -stats-interval 1s \
  >"$receiver_log" 2>&1 &
receiver_pid=$!

listen_deadline=$(( $(now_us) + 5 * 1000000 ))
until receiver_listening; do
  (( $(now_us) < listen_deadline ))
  kill -0 "$receiver_pid"
  sleep 0.05
done

DOUBLETAKE_CODE=4821 \
  "$sender_bin" -target 127.0.0.1 -port 17000 \
  -test -no-audio -hwaccel openh264 -fps 30 -debug \
  -creds "$test_root/pairing.json" >"$sender_log" 2>&1 &
sender_pid=$!

wait_phase setup "$setup_deadline_sec" setup_progress 1
wait_phase media "$media_deadline_sec" media_progress 1

kill -TERM "$sender_pid"
# Sender may exit as soon as it has sent teardown; do not require it to stay up.
wait_phase teardown "$teardown_deadline_sec" teardown_progress 0

cleanup_deadline=$(( $(now_us) + force_cleanup_deadline_sec * 1000000 ))
while kill -0 "$sender_pid" 2>/dev/null; do
  if (( $(now_us) >= cleanup_deadline )); then
    echo "airplay force-cleanup deadline exceeded (${force_cleanup_deadline_sec}s)" >&2
    kill -KILL "$sender_pid" 2>/dev/null || true
    wait "$sender_pid" 2>/dev/null || true
    sender_pid=""
    exit 1
  fi
  sleep 0.05
done
wait "$sender_pid" 2>/dev/null || true
sender_pid=""

kill -TERM "$receiver_pid"
wait "$receiver_pid" 2>/dev/null || true
receiver_pid=""

grep -Eq 'pairing=[1-9][0-9]*/[1-9][0-9]*' "$receiver_log"
grep -Eq 'fairplay=[1-9][0-9]*' "$receiver_log"
grep -Eq 'setup=[1-9][0-9]*' "$receiver_log"
grep -Eq 'video=[1-9][0-9]*/[1-9][0-9]*B' "$receiver_log"
grep -Eq 'teardown=[1-9][0-9]*' "$receiver_log"
