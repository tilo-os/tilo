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
# the verdict. The cases share nothing but the stand-ins, so they all run at
# once, each in a directory of its own, and the suite takes as long as its
# longest case rather than the sum of their windows. What only a booted machine can show (the real greeter's record,
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
# the display devices are what the case's drm directory holds (one card unless
# a case says none), and the greeter's records are what start below writes.
cat >"$bin/systemctl" <<'EOF'
#!/usr/bin/env bash
count_file="$CASE/restart-looks"
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

# Starts one case in the background in its own directory:
#   start ID RECORD INPUT DRM [VAR=VALUE ...]
# RECORD  presented|not-presented|none  the greeter's startup record
# INPUT   keyboard|touch|pointer|none   what the greeter recorded the kiosk gave
#         it (a mouse alone, or no input at all, records nothing)
# DRM     card|none                     the display devices present
# The VAR=VALUE pairs override the greetd history above.
start() {
  local id="$1" record="$2" input="$3" drm="$4" case="$work/$1"
  shift 4
  mkdir -p "$case/drm"
  [ "$drm" = none ] || mkdir -p "$case/drm/card0"
  case "$record" in
    presented)
      printf '{\n  "events": [\n    { "name": "greeter-main-start", "ns": 0 },\n    { "name": "first-frame-presented", "ns": 412000000 }\n  ]\n}\n' >"$case/startup-metrics.json" ;;
    not-presented)
      printf '{\n  "events": [\n    { "name": "greeter-main-start", "ns": 0 }\n  ]\n}\n' >"$case/startup-metrics.json" ;;
  esac
  [ "$input" = none ] || printf '%s\n' "$input" >"$case/greeter-input"
  (
    out="$(env -i PATH="$bin:/usr/bin:/bin" CASE="$case" \
      ACTIVE=yes RESTARTS=0 SESSION=none VIRT=kvm TILO_DRM_SYSFS="$case/drm" \
      TILO_BOOT_HEALTH_BOARD_MARKER="$case/no-board" \
      TILO_BOOT_HEALTH_GREETER_RECORD="$case/startup-metrics.json" \
      TILO_BOOT_HEALTH_GREETER_INPUT="$case/greeter-input" \
      TILO_BOOT_HEALTH_TIMEOUT=6 TILO_BOOT_HEALTH_SETTLE=3 TILO_BOOT_HEALTH_POLL=1 \
      TILO_BOOT_HEALTH_INPUT_WAIT=2 \
      "$@" timeout --kill-after=2 30 bash "$health_script" 2>&1)"
    echo "$?" >"$case/status.part"
    printf '%s' "$out" >"$case/out"
    mv "$case/status.part" "$case/status"
  ) &
}

# The verdict of case ID: its exit status in $status and its output in $out.
judge() {
  status="$(cat "$work/$1/status")"
  out="$(cat "$work/$1/out")"
}

start keyboard presented keyboard card
start touch presented touch card
start no-input presented none card
start pointer presented pointer card
start signed-in presented none card SESSION=user
start closing presented none card SESSION=user STATE=closing
start crash-loop presented keyboard card RESTARTS=loop
start never-presented not-presented keyboard card
start loop-no-record none keyboard card RESTARTS=loop
start settled presented keyboard card RESTARTS=settle:3
start autologin none keyboard card SESSION=user
start greeter-session none keyboard card SESSION=greeter
start greetd-failed presented keyboard card ACTIVE=failed
start other-unit none keyboard card RESTARTS=loop TILO_BOOT_HEALTH_UNIT=multi-user.target
start no-display none keyboard none RESTARTS=loop VIRT=
start virt-none none keyboard none RESTARTS=loop VIRT=none
start headless-vm none keyboard none RESTARTS=loop VIRT=apple
start headless-vm-inactive none keyboard none RESTARTS=loop VIRT=qemu ACTIVE=no TILO_BOOT_HEALTH_TIMEOUT=2
wait

judge keyboard
[ "$status" = 0 ] && grep -q 'a keyboard reaches the sign-in screen' <<<"$out" \
  && ok "a greeter that holds with a keyboard reaching it is a good boot, nobody signed in" \
  || bad "a greeter with a keyboard was judged '$status': $out"

judge touch
[ "$status" = 0 ] && grep -q 'a touch screen reaches the sign-in screen' <<<"$out" \
  && ok "a touch screen reaching the greeter is enough as well" \
  || bad "a greeter with a touch screen was judged '$status': $out"

judge no-input
[ "$status" != 0 ] && [ "$status" != 124 ] && grep -q 'no keyboard or touch screen reached' <<<"$out" \
  && ok "a greeter that holds with no keyboard or touch screen is a failed boot" \
  || bad "a greeter nobody can type into was judged '$status': $out"

judge pointer
[ "$status" != 0 ] && [ "$status" != 124 ] \
  && ok "a record naming anything but a keyboard or touch screen does not count" \
  || bad "a record of 'pointer' was judged '$status': $out"

judge signed-in
[ "$status" = 0 ] && grep -q 'session 7 is signed in through greetd' <<<"$out" \
  && ok "a person who signed in confirms the boot, whatever the input check would say" \
  || bad "a signed-in session was judged '$status': $out"

judge closing
[ "$status" != 0 ] && [ "$status" != 124 ] \
  && ok "a session that is already ending is not a sign-in" \
  || bad "a closing session counted: $out"

judge crash-loop
[ "$status" != 0 ] && grep -q 'did not hold' <<<"$out" \
  && ok "a greeter that presents but keeps restarting is a failed boot" \
  || bad "a crash-looping greeter was judged '$status': $out"

judge never-presented
[ "$status" != 0 ] && grep -q 'no greeter presented a frame' <<<"$out" \
  && ok "greetd active with a greeter that never presents is a failed boot" \
  || bad "a greeter that never drew was judged '$status': $out"

judge loop-no-record
[ "$status" != 0 ] && ok "greetd active and restarting, with no greeter record at all, is a failed boot" \
  || bad "a crash loop with no record was judged good: $out"

judge settled
[ "$status" = 0 ] && ok "restarts that stop, then a greeter that holds, still make a good boot" \
  || bad "a greeter that settled after early restarts was judged failed: $out"

judge autologin
[ "$status" = 0 ] && ok "a person signed in through greetd (first-boot autologin) makes a good boot" \
  || bad "a greetd user session was judged failed: $out"

judge greeter-session
[ "$status" != 0 ] && [ "$status" != 124 ] && ok "the greeter's own logind session is not a signed-in person" \
  || bad "a greeter-class session counted as a sign-in: $out"

judge greetd-failed
[ "$status" != 0 ] && grep -q 'greetd.service FAILED' <<<"$out" \
  && ok "a failed greetd is still a failed boot" || bad "a failed greetd was judged '$status': $out"

# A unit that is not the greeter keeps its plain is-active verdict: the boot
# chain's probe image and headless boards have no greeter to present.
judge other-unit
[ "$status" = 0 ] && ok "a machine judged by another unit is not asked for a greeter" \
  || bad "a non-greeter unit was asked for a greeter: $out"

# No display device at all. On real hardware that is what a broken GPU driver
# leaves behind, so the greeter is still required; a virtual machine made
# without a display (a headless dev VM) has nowhere to present and keeps the
# plain verdict instead of failing every boot of every generation.
judge no-display
[ "$status" != 0 ] && ok "real hardware with no display device and a crash-looping greeter is a failed boot" \
  || bad "a machine whose display stack is gone was judged good: $out"
judge virt-none
[ "$status" != 0 ] && ok "a machine systemd calls 'none' is real hardware too" \
  || bad "virtualization 'none' was taken for a virtual machine: $out"
judge headless-vm
[ "$status" = 0 ] && grep -q 'no display device' <<<"$out" \
  && ok "a virtual machine without a display device is judged by greetd being active" \
  || bad "a headless virtual machine was asked for a greeter: $out"
judge headless-vm-inactive
[ "$status" != 0 ] && ok "and still fails when greetd never becomes active" \
  || bad "a headless virtual machine without greetd was judged good: $out"

exit "$fail"
