#!/usr/bin/env bash
# The always-on board session (os/dev/install-board-layer.sh) against fakes:
# display detection, the session following the display, the network and
# user-manager waits, and boot assessment by that session instead of greetd.
set -uo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
. "$repo/tools/desktop-isolation.sh"
tilo_test_isolate "$@"
libexec="$repo/os/usr/libexec"
unit="$repo/os/usr/lib/systemd/system/tilo-board-headless-session@.service"
fail=0
ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
bin="$work/bin"
mkdir -p "$bin"

# --- tilo-display-present -------------------------------------------------
present() { TILO_DRM_SYSFS="$work/drm" "$libexec/tilo-display-present"; }
connector() { mkdir -p "$work/drm/$1"; printf '%s\n' "$2" >"$work/drm/$1/status"; }
rm -rf "$work/drm"; mkdir -p "$work/drm/card0"
if present; then ok 'no connectors counts as a display'; else bad 'a machine without connectors was judged headless'; fi
connector card0-Writeback-1 connected
connector card0-HDMI-A-1 disconnected
if present; then bad 'writeback or a disconnected HDMI counted as a display'; else ok 'writeback and disconnected sinks are no display'; fi
connector card0-HDMI-A-1 connected
if present; then ok 'a connected HDMI sink is a display'; else bad 'a connected HDMI sink was missed'; fi

# --- tilo-seat-session ----------------------------------------------------
cat >"$bin/getent" <<'EOF'
#!/bin/sh
[ "$1" = passwd ] && [ "$2" = you ] && { echo "you:x:1000:1000::/var/home/you:/bin/bash"; exit 0; }
exit 2
EOF
cat >"$bin/id" <<'EOF'
#!/bin/sh
echo 1000
EOF
cat >"$bin/install" <<'EOF'
#!/bin/sh
echo "install $*" >>"$SEAT_LOG"
EOF
cat >"$bin/systemctl" <<'EOF'
#!/bin/sh
echo "systemctl $*" >>"$SEAT_LOG"
case "$*" in *"stop tilo-board-"*) : >"$SEAT_STOP" ;; esac
EOF
# Each session runs until its stop file appears or for SESSION_SECS.
cat >"$bin/systemd-run" <<'EOF'
#!/bin/bash
backend=${*: -1}
echo "run $backend $*" >>"$SEAT_LOG"
n=0
while [ "$n" -lt "${SESSION_TENTHS:-30}" ]; do
  [ -e "$SEAT_STOP" ] && { rm -f "$SEAT_STOP"; exit 0; }
  sleep 0.1; n=$((n + 1))
done
exit "${SESSION_STATUS:-0}"
EOF
chmod 755 "$bin"/*
cat >"$bin/display" <<'EOF'
#!/bin/sh
[ "$(cat "$DISPLAY_STATE")" = on ]
EOF
chmod 755 "$bin/display"

# Run as `seat ... &`: exec makes $! the supervisor itself, so the test's
# TERM reaches it.
seat() {
  exec env SEAT_LOG="$work/seat.log" SEAT_STOP="$work/stop" DISPLAY_STATE="$work/display" \
    TILO_SEAT_GETENT="$bin/getent" TILO_SEAT_ID="$bin/id" TILO_SEAT_INSTALL="$bin/install" \
    TILO_SEAT_SYSTEMD_RUN="$bin/systemd-run" TILO_SEAT_SYSTEMCTL="$bin/systemctl" \
    TILO_DISPLAY_PRESENT="$bin/display" TILO_SEAT_POLL_SEC=0.1 \
    "$@" "$libexec/tilo-seat-session" you /usr/libexec/tilo-session
}

: >"$work/seat.log"; echo off >"$work/display"; rm -f "$work/stop"
seat SESSION_TENTHS=600 &
pid=$!
sleep 1
echo on >"$work/display"
sleep 1.5
echo off >"$work/display"
sleep 1.5
kill -TERM "$pid"; wait "$pid"
backends="$(awk '$1 == "run" { print $2 }' "$work/seat.log" | tr '\n' ' ')"
if [ "$backends" = 'headless drm headless ' ] \
   && grep -q 'run drm .*--property=PAMName=greetd.*--setenv=XDG_SEAT=seat0' "$work/seat.log" \
   && grep -q 'run headless .*--machine=you@ --user' "$work/seat.log" \
   && grep -q 'systemctl stop getty@tty1.service' "$work/seat.log" \
   && grep -q 'systemctl start getty@tty1.service' "$work/seat.log"; then
  ok 'the session follows the display: headless, drm, headless'
else
  bad "the session did not follow the display: $backends"; cat "$work/seat.log" >&2
fi

: >"$work/seat.log"; echo on >"$work/display"; rm -f "$work/stop"
seat SESSION_TENTHS=600 &
pid=$!
sleep 1
: >"$work/stop"
sleep 1
kill -TERM "$pid"; wait "$pid"
backends="$(awk '$1 == "run" { print $2 }' "$work/seat.log" | tr '\n' ' ')"
if [ "$backends" = 'drm headless ' ]; then
  ok 'a drm session that ends with the display connected falls back to headless'
else
  bad "a failed drm session was not replaced by headless: $backends"
fi

: >"$work/seat.log"; echo off >"$work/display"; rm -f "$work/stop"
( seat SESSION_TENTHS=2 SESSION_STATUS=7 )
status=$?
if [ "$status" = 7 ]; then
  ok 'a session that ends without a display change ends the supervisor with its status'
else
  bad "the supervisor exited $status, not the session's 7"
fi

# --- tilo-board-session-prepare -------------------------------------------
cat >"$bin/nmcli" <<'EOF'
#!/bin/sh
[ -n "${FAKE_NM_ACTIVE:-}" ] && printf '%s\n' "$FAKE_NM_ACTIVE"
exit 0
EOF
cat >"$bin/ip" <<'EOF'
#!/bin/sh
[ "${FAKE_HAS_ADDR:-}" = 1 ] && [ "$2" = route ] && echo 'default via 192.0.2.1 dev eth0'
exit 0
EOF
chmod 755 "$bin/nmcli" "$bin/ip"
: >"$work/profile"
mkdir -p "$work/radio/wlan0/wireless" "$work/noradio/eth0"
network() {
  env -i PATH="$bin:/usr/bin:/bin" TILO_BOARD_WIFI_PROFILE="$work/profile" \
    TILO_BOARD_WIFI_WAIT_ITERS=2 TILO_BOARD_ADDR_WAIT_ITERS=1 "$@" \
    "$libexec/tilo-board-session-prepare" network 2>/dev/null
}
if network TILO_BOARD_NET_SYS="$work/radio" FAKE_HAS_ADDR=1 FAKE_NM_ACTIVE=tilo-setup; then
  ok 'the imager Wi-Fi with an address lets the session start'
else
  bad 'an active tilo-setup with an address was not accepted'
fi
if network TILO_BOARD_NET_SYS="$work/radio" FAKE_HAS_ADDR=1 FAKE_NM_ACTIVE=Wired; then
  bad 'another link was accepted while the given Wi-Fi is down'
else
  ok 'with a radio, only the imager Wi-Fi counts'
fi
if network TILO_BOARD_NET_SYS="$work/noradio" FAKE_HAS_ADDR=1; then
  ok 'without a radio any address counts, so a VM proving the disk starts'
else
  bad 'a machine without a radio waited for Wi-Fi'
fi
if network TILO_BOARD_NET_SYS="$work/noradio" TILO_BOARD_WIFI_PROFILE="$work/missing"; then
  ok 'without a profile the session starts after the wait even with no address'
else
  bad 'a board without a network did not start its session'
fi

cat >"$bin/systemctl" <<'EOF'
#!/bin/sh
echo "systemctl $*" >>"$PREP_LOG"
case "$1" in
  show) echo 4242 ;;
esac
exit 0
EOF
cat >"$bin/id" <<'EOF'
#!/bin/sh
case "$1" in -nG) echo "you wheel tilo-identity" ;; -u) echo 1000 ;; esac
EOF
cat >"$bin/getent" <<'EOF'
#!/bin/sh
[ "$1 $2" = "group tilo-identity" ] && echo "tilo-identity:x:984:" && exit 0
exit 2
EOF
chmod 755 "$bin"/*
mkdir -p "$work/proc/4242"
user_manager() {
  : >"$work/prep.log"
  env -i PATH="$bin:/usr/bin:/bin" PREP_LOG="$work/prep.log" TILO_BOARD_SYSTEMCTL="$bin/systemctl" \
    TILO_BOARD_PROC="$work/proc" TILO_BOARD_IDENTITY_READY="$work/none" TILO_BOARD_USER_WAIT_ITERS=2 \
    "$libexec/tilo-board-session-prepare" user-manager you
}
printf 'Name:\tsystemd\nGroups:\t10 984 1000\n' >"$work/proc/4242/status"
if user_manager && ! grep -q try-restart "$work/prep.log"; then
  ok 'a user manager that already has tilo-identity is left running'
else
  bad 'a user manager with tilo-identity was restarted'
fi
printf 'Name:\tsystemd\nGroups:\t10 1000\n' >"$work/proc/4242/status"
if user_manager && grep -q 'try-restart user@1000.service' "$work/prep.log"; then
  ok 'a user manager started before the account joined tilo-identity is restarted'
else
  bad 'a user manager without tilo-identity was not restarted'
fi

# --- tilo-boot-health -----------------------------------------------------
mkdir -p "$work/wants" "$work/marker"
: >"$work/marker/board-headless-session"
cat >"$bin/systemctl" <<'EOF'
#!/bin/sh
echo "$*" >>"$HEALTH_LOG"
case "$1" in cat|is-active|--failed) exit 0 ;; esac
exit 1
EOF
chmod 755 "$bin/systemctl"
health() {
  : >"$work/health.log"
  env -i PATH="$bin:/usr/bin:/bin" HEALTH_LOG="$work/health.log" \
    TILO_BOOT_HEALTH_BOARD_MARKER="$work/marker/board-headless-session" \
    TILO_BOOT_HEALTH_WANTS="$work/wants" "$@" "$libexec/tilo-boot-health" >/dev/null 2>&1
}
health
if grep -q 'is-active --quiet multi-user.target' "$work/health.log"; then
  ok 'a board without an account is judged by multi-user.target'
else
  bad 'a board without an account waited for something else'
fi
ln -s /usr/lib/systemd/system/tilo-board-headless-session@.service \
  "$work/wants/tilo-board-headless-session@you.service"
health
if grep -q 'is-active --quiet tilo-board-headless-session@you.service' "$work/health.log" \
   && ! grep -q greetd "$work/health.log"; then
  ok 'a board with an account is judged by its session, not the masked greeter'
else
  bad 'a board boot was not judged by its session'
fi
health TILO_BOOT_HEALTH_BOARD_MARKER="$work/missing"
if grep -q 'is-active --quiet greetd.service' "$work/health.log"; then
  ok 'a desktop is still judged by greetd'
else
  bad 'a desktop was not judged by greetd'
fi

# --- unit and image wiring -----------------------------------------------
for step in 'user-manager %i' network clock; do
  grep -Fqx "ExecStartPre=/usr/libexec/tilo-board-session-prepare $step" "$unit" \
    || bad "the board unit does not run the $step wait"
done
grep -Fqx 'ExecStart=/usr/libexec/tilo-seat-session %i /usr/libexec/tilo-session' "$unit" \
  || bad 'the board unit does not run tilo-seat-session'
grep -Fq 'tilo-board-headless-session@$u_name.service' "$repo/os/dev/disk-image.sh" \
  || bad 'disk-image.sh does not enable the board session for its account'
grep -Fq 'install-board-layer.sh /run/tilo-board' "$repo/os/Containerfile.board" \
  || bad 'Containerfile.board does not install the board layer'
for path in os/dev/install-board-layer.sh \
    os/usr/lib/systemd/system/sshd.service.d/20-tilo-board.conf \
    os/etc/NetworkManager/conf.d/10-tilo-brcmfmac.conf \
    os/usr/lib/systemd/system/tilo-board-headless-session@.service \
    os/usr/libexec/tilo-board-session-prepare os/usr/libexec/tilo-seat-session \
    os/usr/libexec/tilo-display-present; do
  [ -e "$repo/$path" ] || bad "$path is missing"
  grep -Fq "$path" "$repo/os/Containerfile.board" || bad "Containerfile.board does not copy $path"
  grep -Fqx "!$path" "$repo/os/Containerfile.board.dockerignore" \
    || bad "the board build context leaves out $path"
  grep -Fq "$path" "$repo/os/dev/install-board-layer.sh" || [ "$path" = os/dev/install-board-layer.sh ] \
    || grep -Fq "${path##*/}" "$repo/os/dev/install-board-layer.sh" \
    || bad "install-board-layer.sh does not install $path"
done
[ "$fail" = 0 ] && ok 'board session wiring'

if [ "$fail" -ne 0 ]; then
  printf 'board session: FAIL\n' >&2
  exit 1
fi
printf 'board session: PASS\n'
