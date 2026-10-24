#!/usr/bin/env bash
# tilo-boot-health judges a desktop boot by a greeter that is on the screen and
# stays there, not by greetd being active, and then by a keyboard or touch
# screen reaching it, not by the system having started. It never waits for a
# person, but a person signing in settles it too.
#
#   ./os/tests/boot_health_greeter.sh
#
# greetd has Restart=, so it is active between restarts; a greeter that cannot
# draw crash-loops while greetd looks fine. Each case runs the production script
# against systemctl/loginctl stand-ins that play one greetd history,
# with the windows shortened (SETTLE 3 s, deadline 6 s, 1 s polls), and asserts
# the verdict. What only a booted machine can show (the real greeter's record,
# the boot loader spending tries) is os/tests/boot_assessment_rollback.sh.
set -uo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
health_script="$repo/os/usr/libexec/tilo-boot-health"
fail=0
ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
bin="$work/bin"
mkdir -p "$bin"

# One greetd history per case, read from the environment:
#   ACTIVE      yes|no|failed   what is-active / is-failed say
#   RESTARTS    fixed           NRestarts stays at this value
#               loop            NRestarts goes up on every look (a crash loop)
#               settle:N        goes up on every look until the Nth, then stays
#   SESSION     user|greeter|none   the class of a second greetd session, 7,
#               beside the greeter's own session 3
#   STATE       the logind state of session 7 (active unless a case says)
#   VIRT        what systemd detected the machine to be ('' on real hardware)
# the display devices are what $work/drm holds (one card unless a case empties
# it), and the greeter's input record is what greeter_input below writes.
cat >"$bin/systemctl" <<'EOF'
#!/usr/bin/env bash
count_file="$WORK/restart-looks"
case "$1" in
  cat) exit 0 ;;
  is-active) [ "$ACTIVE" = yes ] ;;
  is-failed) [ "$ACTIVE" = failed ] ;;
  show)
    looks=$(( $(cat "$count_file" 2>/dev/null || echo 0) + 1 ))
    echo "$looks" >"$count_file"
    case "$RESTARTS" in
      loop) n="$looks" ;;
      settle:*) limit="${RESTARTS#settle:}"; n=$(( looks < limit ? looks : limit )) ;;
      *) n="$RESTARTS" ;;
    esac
    for arg in "$@"; do
      case "$arg" in
        NRestarts) echo "$n" ;;
        MainPID) echo 4242 ;;
        Virtualization) echo "$VIRT" ;;
      esac
    done ;;
  *) exit 0 ;;
esac
EOF
cat >"$bin/loginctl" <<'EOF'
#!/usr/bin/env bash
case "$1" in
  list-sessions)
    echo "3 976 greeter seat0 tty1 active no -"
    [ "$SESSION" = none ] || echo "7 1000 you seat0 tty1 active no -" ;;
  show-session)
    if [ "$2" = 3 ]; then
      case "$4" in Service) echo greetd ;; Class) echo greeter ;; State) echo active ;; esac
    else
      case "$4" in Service) echo greetd ;; Class) echo "$SESSION" ;; State) echo "${STATE:-active}" ;; esac
    fi ;;
esac
EOF
chmod 755 "$bin/systemctl" "$bin/loginctl"

mkdir -p "$work/drm/card0"
record="$work/startup-metrics.json"
presented() {
  printf '{\n  "events": [\n    { "name": "greeter-main-start", "ns": 0 },\n    { "name": "first-frame-presented", "ns": 412000000 }\n  ]\n}\n' >"$record"
}
not_presented() {
  printf '{\n  "events": [\n    { "name": "greeter-main-start", "ns": 0 }\n  ]\n}\n' >"$record"
}

# What the greeter recorded the kiosk gave it: keyboard, touch, something
# else, or nothing (a mouse alone, or no input at all, records nothing).
input_record="$work/greeter-input"
greeter_input() {
  rm -f "$input_record"
  [ "$1" = none ] || printf '%s\n' "$1" >"$input_record"
}

# Runs the script; leaves its exit status in $status and its output in $out.
judge() {
  rm -f "$work/restart-looks"
  out="$(env -i PATH="$bin:/usr/bin:/bin" WORK="$work" \
    ACTIVE=yes RESTARTS=0 SESSION=none VIRT=kvm TILO_DRM_SYSFS="$work/drm" \
    TILO_BOOT_HEALTH_BOARD_MARKER="$work/no-board" \
    TILO_BOOT_HEALTH_GREETER_RECORD="$record" \
    TILO_BOOT_HEALTH_GREETER_INPUT="$input_record" \
    TILO_BOOT_HEALTH_TIMEOUT=6 TILO_BOOT_HEALTH_SETTLE=3 TILO_BOOT_HEALTH_POLL=1 \
    TILO_BOOT_HEALTH_INPUT_WAIT=2 \
    "$@" timeout --kill-after=2 30 bash "$health_script" 2>&1)"
  status=$?
}

presented
greeter_input keyboard
judge
[ "$status" = 0 ] && grep -q 'a keyboard reaches the sign-in screen' <<<"$out" \
  && ok "a greeter that holds with a keyboard reaching it is a good boot, nobody signed in" \
  || bad "a greeter with a keyboard was judged '$status': $out"

greeter_input touch
judge
[ "$status" = 0 ] && grep -q 'a touch screen reaches the sign-in screen' <<<"$out" \
  && ok "a touch screen reaching the greeter is enough as well" \
  || bad "a greeter with a touch screen was judged '$status': $out"

greeter_input none
judge
[ "$status" != 0 ] && [ "$status" != 124 ] && grep -q 'no keyboard or touch screen reached' <<<"$out" \
  && ok "a greeter that holds with no keyboard or touch screen is a failed boot" \
  || bad "a greeter nobody can type into was judged '$status': $out"

greeter_input pointer
judge
[ "$status" != 0 ] && [ "$status" != 124 ] \
  && ok "a record naming anything but a keyboard or touch screen does not count" \
  || bad "a record of '$(cat "$input_record")' was judged '$status': $out"

greeter_input none
judge SESSION=user
[ "$status" = 0 ] && grep -q 'session 7 is signed in through greetd' <<<"$out" \
  && ok "a person who signed in confirms the boot, whatever the input check would say" \
  || bad "a signed-in session was judged '$status': $out"

greeter_input none
judge SESSION=user STATE=closing
[ "$status" != 0 ] && [ "$status" != 124 ] \
  && ok "a session that is already ending is not a sign-in" \
  || bad "a closing session counted: $out"

greeter_input keyboard
presented
judge RESTARTS=loop
[ "$status" != 0 ] && grep -q 'did not hold' <<<"$out" \
  && ok "a greeter that presents but keeps restarting is a failed boot" \
  || bad "a crash-looping greeter was judged '$status': $out"

not_presented
judge
[ "$status" != 0 ] && grep -q 'no greeter presented a frame' <<<"$out" \
  && ok "greetd active with a greeter that never presents is a failed boot" \
  || bad "a greeter that never drew was judged '$status': $out"

rm -f "$record"
judge RESTARTS=loop
[ "$status" != 0 ] && ok "greetd active and restarting, with no greeter record at all, is a failed boot" \
  || bad "a crash loop with no record was judged good: $out"

presented
judge RESTARTS=settle:3
[ "$status" = 0 ] && ok "restarts that stop, then a greeter that holds, still make a good boot" \
  || bad "a greeter that settled after early restarts was judged failed: $out"

rm -f "$record"
judge SESSION=user
[ "$status" = 0 ] && ok "a person signed in through greetd (first-boot autologin) makes a good boot" \
  || bad "a greetd user session was judged failed: $out"

rm -f "$record"
judge SESSION=greeter
[ "$status" != 0 ] && [ "$status" != 124 ] && ok "the greeter's own logind session is not a signed-in person" \
  || bad "a greeter-class session counted as a sign-in: $out"

presented
judge ACTIVE=failed
[ "$status" != 0 ] && grep -q 'greetd.service FAILED' <<<"$out" \
  && ok "a failed greetd is still a failed boot" || bad "a failed greetd was judged '$status': $out"

# A unit that is not the greeter keeps its plain is-active verdict: the boot
# chain's probe image and headless boards have no greeter to present.
rm -f "$record"
judge RESTARTS=loop TILO_BOOT_HEALTH_UNIT=multi-user.target
[ "$status" = 0 ] && ok "a machine judged by another unit is not asked for a greeter" \
  || bad "a non-greeter unit was asked for a greeter: $out"

# No display device at all. On real hardware that is what a broken GPU driver
# leaves behind, so the greeter is still required; a virtual machine made
# without a display (a headless dev VM) has nowhere to present and keeps the
# plain verdict instead of failing every boot of every generation.
rm -rf "$work/drm/card0"
rm -f "$record"
judge RESTARTS=loop VIRT=
[ "$status" != 0 ] && ok "real hardware with no display device and a crash-looping greeter is a failed boot" \
  || bad "a machine whose display stack is gone was judged good: $out"
judge RESTARTS=loop VIRT=none
[ "$status" != 0 ] && ok "a machine systemd calls 'none' is real hardware too" \
  || bad "virtualization 'none' was taken for a virtual machine: $out"
judge RESTARTS=loop VIRT=apple
[ "$status" = 0 ] && grep -q 'no display device' <<<"$out" \
  && ok "a virtual machine without a display device is judged by greetd being active" \
  || bad "a headless virtual machine was asked for a greeter: $out"
judge RESTARTS=loop VIRT=qemu ACTIVE=no TILO_BOOT_HEALTH_TIMEOUT=2
[ "$status" != 0 ] && ok "and still fails when greetd never becomes active" \
  || bad "a headless virtual machine without greetd was judged good: $out"
mkdir -p "$work/drm/card0"

exit "$fail"
