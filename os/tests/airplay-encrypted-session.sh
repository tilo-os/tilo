#!/usr/bin/env bash
set -euo pipefail

test_root="$(mktemp -d)"
receiver_pid=""
cleanup() {
  if [[ -n "$receiver_pid" ]]; then
    kill -TERM "$receiver_pid" 2>/dev/null || true
    wait "$receiver_pid" 2>/dev/null || true
  fi
  rm -rf -- "$test_root"
}
trap cleanup EXIT

receiver_log="$test_root/receiver.log"
sender_log="$test_root/sender.log"
receiver_bin="${QOS_DOUBLETAKE_TEST_RECEIVER:-/usr/libexec/qos-doubletake-test-receiver}"
sender_bin="${QOS_DOUBLETAKE:-/usr/libexec/qos-doubletake}"
DOUBLETAKE_RECEIVER_CODE=4821 \
  "$receiver_bin" \
  -listen 127.0.0.1:17000 -profile modern -auth pin -stats-interval 1s \
  >"$receiver_log" 2>&1 &
receiver_pid=$!

for _ in {1..50}; do
  grep -q "listening on 127.0.0.1:17000" "$receiver_log" && break
  kill -0 "$receiver_pid"
  sleep 0.1
done
grep -q "listening on 127.0.0.1:17000" "$receiver_log"

set +e
DOUBLETAKE_CODE=4821 timeout --signal=TERM 12s \
  "$sender_bin" -target 127.0.0.1 -port 17000 \
  -test -no-audio -hwaccel openh264 -fps 30 \
  -creds "$test_root/pairing.json" >"$sender_log" 2>&1
sender_status=$?
set -e
[[ "$sender_status" == 124 || "$sender_status" == 143 ]]

kill -TERM "$receiver_pid"
wait "$receiver_pid"
receiver_pid=""

grep -Eq 'pairing=[1-9][0-9]*/[1-9][0-9]*' "$receiver_log"
grep -Eq 'fairplay=[1-9][0-9]*' "$receiver_log"
grep -Eq 'setup=[1-9][0-9]*' "$receiver_log"
grep -Eq 'video=[1-9][0-9]*/[1-9][0-9]*B' "$receiver_log"
grep -Eq 'teardown=[1-9][0-9]*' "$receiver_log"
