#!/usr/bin/env bash
# Hermetic coverage for airplay-encrypted-session.sh phase deadlines.
# Drives the production script with fixture binaries so each named deadline is
# observed without an assembled image or real doubletake/OpenH264 stack.
set -euo pipefail

repo="$(cd "$(dirname "$0")/../.." && pwd)"
script="$repo/os/tests/airplay-encrypted-session.sh"
containerfile="$repo/os/Containerfile"

test -x "$script"
test -f "$containerfile"

# Assembly must keep the script and must not treat its runtime as an install gate.
grep -Fq 'COPY os/tests/airplay-encrypted-session.sh /usr/libexec/tilo/airplay-encrypted-session-test.sh' \
  "$containerfile"
grep -Fq 'Optional AirPlay media acceptance runs against the assembled image separately.' \
  "$containerfile"
# The former retrying install-time invocation must stay gone.
if grep -n 'airplay-encrypted-session-test\.sh' "$containerfile" | grep -Eq '\|\|'; then
  echo 'Containerfile still runs AirPlay acceptance during assembly' >&2
  exit 1
fi

work="$(mktemp -d)"
finish() {
  status=$?
  if (( status != 0 )); then
    # The production script's output explains which phase ended the run.
    for out in stdout stderr; do
      if [[ -s "$work/$out" ]]; then
        echo "--- airplay-encrypted-session.sh $out ---" >&2
        sed 's/^/  /' "$work/$out" >&2
      fi
    done
  fi
  rm -rf "$work"
  exit "$status"
}
trap finish EXIT
mkdir -p "$work/bin"

write_gst_ok() {
  cat >"$work/bin/gst-inspect" <<'BIN'
#!/usr/bin/env bash
exit 0
BIN
  chmod +x "$work/bin/gst-inspect"
}

write_gst_hang() {
  cat >"$work/bin/gst-inspect" <<'BIN'
#!/usr/bin/env bash
sleep 1000
BIN
  chmod +x "$work/bin/gst-inspect"
}

write_gst_missing() {
  cat >"$work/bin/gst-inspect" <<'BIN'
#!/usr/bin/env bash
exit 1
BIN
  chmod +x "$work/bin/gst-inspect"
}

# Fixture receiver/sender share $TILO_AIRPLAY_FIXTURE_DIR.
# SCENARIO controls which stats lines appear.
write_receiver() {
  cat >"$work/bin/receiver" <<'BIN'
#!/usr/bin/env bash
set -euo pipefail
dir="${TILO_AIRPLAY_FIXTURE_DIR:?}"
scenario="${TILO_AIRPLAY_FIXTURE_SCENARIO:?}"
echo "listening on 127.0.0.1:17000"
# Pairing needs a connected sender: report nothing until one is ready for
# its signals, as the real receiver would.
while [[ ! -f "$dir/sender-ready" ]]; do
  sleep 0.05
done
case "$scenario" in
  happy|teardown-stall|force-cleanup|media-stall)
    echo "stats: connections=1 info=1 pairing=1/1 fairplay=1 setup=1 feedback=0 teardown=0 events=0 video=0/0B audio=0/0B timing=0/0 digest_challenges=0"
    ;;
  setup-stall)
    echo "stats: connections=1 info=1 pairing=0/0 fairplay=0 setup=0 feedback=0 teardown=0 events=0 video=0/0B audio=0/0B timing=0/0 digest_challenges=0"
    ;;
  *)
    echo "unknown scenario $scenario" >&2
    exit 1
    ;;
esac
case "$scenario" in
  happy|teardown-stall|force-cleanup)
    echo "stats: connections=1 info=1 pairing=1/1 fairplay=1 setup=1 feedback=1 teardown=0 events=0 video=8/2048B audio=0/0B timing=0/0 digest_challenges=0"
    ;;
  media-stall)
    # Setup complete, media never progresses.
    ;;
esac
case "$scenario" in
  happy)
    while [[ ! -f "$dir/sender-term" ]]; do
      sleep 0.05
    done
    echo "stats: connections=1 info=1 pairing=1/1 fairplay=1 setup=1 feedback=1 teardown=1 events=0 video=8/2048B audio=0/0B timing=0/0 digest_challenges=0"
    ;;
  force-cleanup)
    # Teardown is observed so the force-cleanup deadline is the one that fires
    # when the sender ignores TERM.
    echo "stats: connections=1 info=1 pairing=1/1 fairplay=1 setup=1 feedback=1 teardown=1 events=0 video=8/2048B audio=0/0B timing=0/0 digest_challenges=0"
    ;;
  teardown-stall)
    # Media present; teardown never arrives.
    ;;
esac
while true; do
  sleep 0.05
done
BIN
  chmod +x "$work/bin/receiver"
}

write_sender() {
  local mode="$1"
  cat >"$work/bin/sender" <<BIN
#!/usr/bin/env bash
set -euo pipefail
dir="\${TILO_AIRPLAY_FIXTURE_DIR:?}"
mode="$mode"
if [[ "\$mode" == "ignore-term" ]]; then
  trap '' TERM
  : >"\$dir/sender-ready"
  while true; do sleep 0.05; done
fi
term_handler() {
  : >"\$dir/sender-term"
  exit 0
}
trap term_handler TERM
: >"\$dir/sender-ready"
# Short sleeps so a deferred bash TERM trap runs promptly.
while true; do
  sleep 0.05
done
BIN
  chmod +x "$work/bin/sender"
}

run_script() {
  TILO_AIRPLAY_FIXTURE_DIR="$work/fixture" \
  TILO_AIRPLAY_FIXTURE_SCENARIO="$1" \
  TILO_AIRPLAY_GST_INSPECT="$work/bin/gst-inspect" \
  TILO_AIRPLAY_GST_LAUNCH="$work/bin/gst-launch" \
  TILO_DOUBLETAKE_TEST_RECEIVER="$work/bin/receiver" \
  TILO_DOUBLETAKE="$work/bin/sender" \
  AIRPLAY_PLUGIN_DEADLINE_SEC="${AIRPLAY_PLUGIN_DEADLINE_SEC:-$ample}" \
  AIRPLAY_CODEC_DEADLINE_SEC="${AIRPLAY_CODEC_DEADLINE_SEC:-$ample}" \
  AIRPLAY_SETUP_DEADLINE_SEC="${AIRPLAY_SETUP_DEADLINE_SEC:-$ample}" \
  AIRPLAY_MEDIA_DEADLINE_SEC="${AIRPLAY_MEDIA_DEADLINE_SEC:-$ample}" \
  AIRPLAY_TEARDOWN_DEADLINE_SEC="${AIRPLAY_TEARDOWN_DEADLINE_SEC:-$ample}" \
  AIRPLAY_FORCE_CLEANUP_DEADLINE_SEC="${AIRPLAY_FORCE_CLEANUP_DEADLINE_SEC:-$ample}" \
  "$script" >"$work/stdout" 2>"$work/stderr"
}

# Phases that must pass get ample time and finish as soon as they progress;
# only the phase a scenario stalls gets a short deadline, so its expiry is the
# one reported however slow the host is.
ample=60
short=1
deadline_var() {
  case "$1" in
    plugin-discovery) echo AIRPLAY_PLUGIN_DEADLINE_SEC ;;
    codec-initialization) echo AIRPLAY_CODEC_DEADLINE_SEC ;;
    setup) echo AIRPLAY_SETUP_DEADLINE_SEC ;;
    media) echo AIRPLAY_MEDIA_DEADLINE_SEC ;;
    teardown) echo AIRPLAY_TEARDOWN_DEADLINE_SEC ;;
    force-cleanup) echo AIRPLAY_FORCE_CLEANUP_DEADLINE_SEC ;;
  esac
}

expect_fail_phase() {
  local scenario="$1"
  local phase="$2"
  rm -rf "$work/fixture"
  mkdir -p "$work/fixture"
  set +e
  (
    export "$(deadline_var "$phase")=$short"
    run_script "$scenario"
  )
  status=$?
  set -e
  if (( status == 0 )); then
    echo "expected $phase deadline failure for scenario $scenario" >&2
    exit 1
  fi
  if ! grep -Fq "airplay ${phase} deadline exceeded" "$work/stderr"; then
    echo "scenario $scenario did not end on the $phase deadline" >&2
    exit 1
  fi
}

write_codec() {
  local mode="$1"
  cat >"$work/bin/gst-launch" <<'BIN'
#!/usr/bin/env bash
set -euo pipefail
case "${TILO_AIRPLAY_FIXTURE_CODEC:?}" in
  hang) sleep 1000 ;;
  stub) exit 1 ;;
  empty) exit 0 ;;
  ok)
    for argument in "$@"; do
      if [[ $argument == location=* ]]; then
        truncate -s 691200 "${argument#location=}"
        exit 0
      fi
    done
    exit 1 ;;
esac
BIN
  chmod +x "$work/bin/gst-launch"
  export TILO_AIRPLAY_FIXTURE_CODEC="$mode"
}
write_codec ok

# --- plugin-discovery hang names its phase ---------------------------------
write_gst_hang
write_receiver
write_sender normal
expect_fail_phase setup-stall plugin-discovery

# --- missing plugin fails closed (not a silent skip) -----------------------
write_gst_missing
rm -rf "$work/fixture"
mkdir -p "$work/fixture"
set +e
TILO_AIRPLAY_FIXTURE_DIR="$work/fixture" \
TILO_AIRPLAY_FIXTURE_SCENARIO=setup-stall \
TILO_AIRPLAY_GST_INSPECT="$work/bin/gst-inspect" \
  TILO_AIRPLAY_GST_LAUNCH="$work/bin/gst-launch" \
TILO_DOUBLETAKE_TEST_RECEIVER="$work/bin/receiver" \
TILO_DOUBLETAKE="$work/bin/sender" \
AIRPLAY_PLUGIN_DEADLINE_SEC=2 \
  "$script" >"$work/stdout" 2>"$work/stderr"
status=$?
set -e
(( status != 0 ))
grep -Fq 'airplay plugin-discovery failed: openh264enc is not available' "$work/stderr"

# Discovery succeeds for a stub; initialization failure or no decoded frames
# must stop before either network participant starts.
write_gst_ok
for codec in stub empty; do
  write_codec "$codec"
  rm -rf "$work/fixture"
  mkdir -p "$work/fixture"
  if run_script happy; then
    echo "expected codec initialization failure for $codec" >&2
    exit 1
  fi
  grep -Fq 'airplay codec-initialization failed:' "$work/stderr"
  test ! -e "$work/fixture/sender-ready"
done
write_codec hang
expect_fail_phase happy codec-initialization
test ! -e "$work/fixture/sender-ready"
write_codec ok

# --- setup / media / teardown / force-cleanup each name their phase --------
write_gst_ok
write_receiver
write_sender normal
expect_fail_phase setup-stall setup
expect_fail_phase media-stall media
expect_fail_phase teardown-stall teardown

write_sender ignore-term
expect_fail_phase force-cleanup force-cleanup

# --- happy path preserves every protocol/media/teardown assertion ----------
write_sender normal
rm -rf "$work/fixture"
mkdir -p "$work/fixture"
# Every phase progresses, so ample bounds cost nothing.
run_script happy

# Production script still declares distinct deadlines (not one coarse timeout).
grep -Eq 'AIRPLAY_SETUP_DEADLINE_SEC:-30' "$script"
grep -Eq 'AIRPLAY_MEDIA_DEADLINE_SEC:-60' "$script"
grep -Eq 'AIRPLAY_TEARDOWN_DEADLINE_SEC:-10' "$script"
grep -Eq 'AIRPLAY_FORCE_CLEANUP_DEADLINE_SEC:-5' "$script"
grep -Eq 'AIRPLAY_PLUGIN_DEADLINE_SEC:-30' "$script"
# The old whole-session 12s timeout must not return.
if grep -Eq 'timeout --signal=TERM 12s' "$script"; then
  echo 'production script still uses a coarse 12s whole-session timeout' >&2
  exit 1
fi

printf 'ok: airplay encrypted-session phase deadlines are observed on the production script path\n'
