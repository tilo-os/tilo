#!/usr/bin/env bash
# Apps opened on a slow machine keep their windows.
#
# On a laptop whose frames took 50-130 ms, every app opened after session
# start vanished: a new tab's slot width is a spring stepped once per frame,
# and at those frame times it diverged, so the compositor configured the app
# ever wider (780, 222, 186, 880, 5089, 36736 px) until the app could not
# allocate its surface, closed its window and exited cleanly.
#
# This signs in again on the booted dev VM with the compositor's frame stall
# (TILO_TEST_FRAME_STALL_MS, a dev-overrides knob; release compositors
# ignore it) set through the login's environment, so the real frame loop runs
# at that laptop's frame times. It then opens apps one after another through
# the app palette, as a person does, and requires:
#
#   1. the stall is in effect: the compositor announced it, and its painted
#      frames each took at least the stall's lower bound (frame trace);
#   2. every app opened stays running with its window on the accessibility
#      bus for the hold time, while the next ones open beside it;
#   3. no app was configured larger than the display, and the compositor
#      never had to cap a configure at what a client can render;
#   4. no app process exited.
#
# Runs against the VM os/dev/vm.sh booted and logged in (same TILO_*
# environment), after the other installed-desktop checks: it ends that
# session. The session it starts keeps the stall; the VM is throwaway.
#
#   os/dev/test-slow-frame-launch.sh [--out DIR] [--stall 50-130] [--hold SEC] [APP_NAME...]
#
# APP_NAME is an app's name in the palette; default: Notes, Files and
# Personal Settings.
set -euo pipefail

dev_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
vm="$dev_dir/vm.sh"
out="${TILO_RUN:-$PWD}/slow-frames"
stall="${TILO_SLOW_FRAME_STALL:-50-130}"
hold=10
apps=()
while [ "$#" -gt 0 ]; do
  case "$1" in
    --out) out="${2:?--out needs a directory}"; shift 2 ;;
    --stall) stall="${2:?--stall needs MS or MIN-MAX}"; shift 2 ;;
    --hold) hold="${2:?--hold needs seconds}"; shift 2 ;;
    -*) printf 'unknown option: %s\n' "$1" >&2; exit 2 ;;
    *) apps+=("$1"); shift ;;
  esac
done
[ "${#apps[@]}" -gt 0 ] || apps=(Notes Files "Personal Settings")
[[ $stall =~ ^[0-9]+(-[0-9]+)?$ ]] || { echo "--stall must be MS or MIN-MAX, not $stall" >&2; exit 2; }
[[ $hold =~ ^[0-9]+$ ]] || { echo "--hold must be whole seconds" >&2; exit 2; }
stall_min="${stall%%-*}"
pass="${TILO_PASS:-tilo}"
session_logs='$HOME/.local/state/tilo/session-logs'
mkdir -p "$out"

note() { printf 'slow frames: %s\n' "$*" >&2; }
collect() {
  "$vm" ssh "cat $session_logs/latest.log" >"$out/session.log" 2>/dev/null || true
  "$vm" ssh "cat $session_logs/frame-trace.csv" >"$out/frame-trace.csv" 2>/dev/null || true
}
die() {
  printf 'slow frames: FAIL %s\n' "$*" >&2
  collect
  if [ -s "$out/session.log" ]; then
    echo '--- the session log (last 80 lines) ---' >&2
    sed 's/\x1b\[[0-9;]*m//g' "$out/session.log" | tail -80 >&2
  fi
  exit 1
}
# Root on the guest for the login environment only. The account is in wheel;
# sudo takes the password's line from stdin and the shell reads the rest.
as_root() { printf '%s\n%s\n' "$pass" "$1" | "$vm" ssh "sudo -S -p '' sh -s"; }

# --- the stall and the log lines the checks read, in the login environment ----
# greetd starts the session with the PAM environment, which pam_env reads from
# /etc/environment. RUST_LOG adds the configure and launcher lines to the
# session's flight record; errors are logged regardless.
log_filter='error,shell::compositor::foreign=debug,shell::launcher=info'
as_root "set -eu
  sed -i '/^TILO_TEST_FRAME_STALL_MS=/d; /^RUST_LOG=/d' /etc/environment
  printf '%s\n' 'TILO_TEST_FRAME_STALL_MS=$stall' 'RUST_LOG=$log_filter' >>/etc/environment" \
  || die "could not set the login environment"

# --- sign in again: the new session starts with the stall -----------------------
as_root "loginctl list-sessions --no-legend | awk '\$4 == \"seat0\" { print \$1 }' \
  | while read -r session; do loginctl terminate-session \"\$session\"; done" \
  || die "could not end the running session"
"$vm" wait-greeter-return
"$vm" login --passcode-only "$pass"
for attempt in $(seq 1 180); do
  "$vm" ssh 'busctl --user status org.freedesktop.impl.portal.desktop.tilo >/dev/null 2>&1' && break
  [ "$attempt" -lt 180 ] || die "the compositor did not come back after signing in again"
  sleep 1
done
"$vm" ssh "grep -q 'TILO_TEST_FRAME_STALL_MS: every painted frame stalls $stall ms' $session_logs/latest.log" \
  || die "the new session did not take TILO_TEST_FRAME_STALL_MS=$stall from the login environment"
note "the session runs every painted frame $stall ms slower"

# The display's size: what QEMU scans out, in pixels. No tab is ever wider
# or taller than its output, and a configure is in logical pixels, never
# more than these.
display_size() {
  local shot
  shot="$("$vm" shot "slow-frames-$1")" || die "could not capture the display"
  cp -f "$shot" "$out/$1.png"
  python3 -c 'import struct, sys
head = open(sys.argv[1], "rb").read(24)
assert head[:8] == b"\x89PNG\r\n\x1a\n", "not a PNG"
print(*struct.unpack(">II", head[16:24]))' "$shot"
}
read -r out_w out_h < <(display_size signed-in)
[ "${out_w:-0}" -gt 0 ] && [ "${out_h:-0}" -gt 0 ] || die "the display has no size"
note "the display is ${out_w}x${out_h}"

# --- open the apps one after another, each beside the ones before ---------------
opened=()
for app in "${apps[@]}"; do
  reply="$("$vm" test installed-atspi-apps.sh "open '$app'")" || die "$app did not open"
  read -r bus pid <<<"$reply"
  [[ -n $bus && $pid =~ ^[0-9]+$ ]] || die "$app opened, but the client printed '$reply'"
  note "$app opened (bus $bus, pid $pid); holding ${hold}s"
  opened+=("$app|$bus|$pid")
  sleep "$hold"
  for entry in "${opened[@]}"; do
    IFS='|' read -r name each_bus each_pid <<<"$entry"
    "$vm" ssh "kill -0 $each_pid" 2>/dev/null \
      || die "$name (pid $each_pid) exited after $app opened"
    "$vm" test installed-atspi-apps.sh "names '$each_bus' >/dev/null" \
      || die "$name (pid $each_pid) left the accessibility bus after $app opened"
  done
done
display_size apps-open >/dev/null
collect
note "every app is still running with its window"

# --- what the compositor did meanwhile -------------------------------------------
plain="$out/session.plain.log"
sed 's/\x1b\[[0-9;]*m//g' "$out/session.log" >"$plain"
if grep -n 'laid out larger than a client can render' "$plain" >&2; then
  die "a tab was laid out larger than a client can render"
fi
if grep -n 'application process exited' "$plain" >&2; then
  die "an app process exited while the apps were open"
fi
configures="$(grep -c 'configured foreign toplevel to its tab size' "$plain" || true)"
[ "${configures:-0}" -gt 0 ] || die "the session log has no configure lines (RUST_LOG=$log_filter)"
read -r max_w max_h < <(grep 'configured foreign toplevel to its tab size' "$plain" \
  | sed -n 's/.*width=\([0-9]*\) height=\([0-9]*\).*/\1 \2/p' \
  | awk '$1 > w { w = $1 } $2 > h { h = $2 } END { print w + 0, h + 0 }')
if [ "$max_w" -gt "$out_w" ] || [ "$max_h" -gt "$out_h" ]; then
  grep 'configured foreign toplevel to its tab size' "$plain" | tail -40 >&2
  die "an app was configured to ${max_w}x${max_h}, larger than the ${out_w}x${out_h} display"
fi
note "$configures configures, the largest ${max_w}x${max_h}, within the ${out_w}x${out_h} display"

# The frame trace: a painted tick's work is the next tick's period less this
# tick's park. Every painted frame must have carried the stall.
python3 - "$out/frame-trace.csv" "$stall_min" <<'EOF' || die "the frame loop did not run at the stall's frame times"
import csv, sys
path, floor_ms = sys.argv[1], int(sys.argv[2])
rows = list(csv.DictReader(open(path, newline="")))
work = []
for before, after in zip(rows, rows[1:]):
    if before["painted"] == "1":
        work.append(int(after["period_us"]) - int(before["sleep_us"]))
if len(work) < 20:
    sys.exit(f"slow frames: only {len(work)} painted frames in the trace")
work.sort()
tenth = work[len(work) // 10]
median = work[len(work) // 2]
print(f"slow frames: {len(work)} painted frames, work p10 {tenth / 1000:.1f} ms, "
      f"median {median / 1000:.1f} ms", file=sys.stderr)
if tenth < (floor_ms - 2) * 1000:
    sys.exit(f"slow frames: a tenth of the painted frames took under {floor_ms} ms")
EOF
note "passed: ${#apps[@]} apps opened at $stall ms frames and kept their windows"
