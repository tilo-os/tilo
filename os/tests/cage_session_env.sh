#!/usr/bin/env bash
# os/usr/libexec/tilo-cage-session picks cage's environment from the display
# hardware: QEMU's virtio-gpu gets the software renderer, legacy modesetting
# and the software cursor; a physical GPU keeps all three of its own, above
# all the hardware cursor plane (a composited software cursor leaves a trail
# of old pointer stamps on real hardware).
set -uo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
wrapper="$repo/os/usr/libexec/tilo-cage-session"
fail=0
ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin"
# A cage stand-in that reports the environment it was started with.
cat >"$work/bin/cage" <<'EOF'
#!/bin/sh
printf 'args=%s\n' "$*"
printf 'umask=%s\n' "$(umask)"
for name in WLR_RENDERER WLR_DRM_NO_ATOMIC WLR_NO_HARDWARE_CURSORS TILO_STARTUP_METRICS_PATH TILO_GREETER_INPUT_RECORD TILO_CAGE_SESSION_KEEP_OUTPUT; do
  eval "value=\${$name-unset}"
  printf '%s=%s\n' "$name" "$value"
done
exit "${CAGE_EXIT:-0}"
EOF
# Report x86_64 so the ARM-only CPU shim branch stays out of this check.
cat >"$work/bin/uname" <<'EOF'
#!/bin/sh
echo x86_64
EOF
chmod +x "$work/bin/cage" "$work/bin/uname"

card() {
  mkdir -p "$work/drm/$1/device"
  printf 'DRIVER=%s\nPCI_ID=%s\n' "$2" "$3" >"$work/drm/$1/device/uevent"
}

mkdir -p "$work/run-greeter"
run_program() {
  env -u WLR_RENDERER -u WLR_DRM_NO_ATOMIC -u WLR_NO_HARDWARE_CURSORS -u TILO_STARTUP_METRICS_PATH -u TILO_GREETER_INPUT_RECORD \
    -u TILO_CAGE_SESSION_KEEP_OUTPUT TILO_JOURNAL_STDOUT_SOCKET="$work/no-journal" \
    PATH="$work/bin:$PATH" TILO_DRM_SYSFS="$work/drm" TILO_CAGE="$work/bin/cage" \
    TILO_GREETER_RECORD_DIR="$work/run-greeter" "$@" bash "$wrapper" "$program"
}
run() { program=/usr/libexec/tilo/tilo-greeter run_program "$@"; }

# --- a physical GPU ------------------------------------------------------
rm -rf "$work/drm"
card card1 msm_dpu ''
out="$(run)"
grep -qx 'args=-s -- /usr/libexec/tilo/tilo-greeter' <<<"$out" \
  && ok 'the greeter runs in cage, which exits with it' || bad "unexpected cage arguments: $out"
grep -qx 'WLR_NO_HARDWARE_CURSORS=unset' <<<"$out" \
  && ok 'a physical GPU keeps the hardware cursor' || bad "physical GPU forced a software cursor: $out"
grep -qx 'WLR_RENDERER=unset' <<<"$out" && grep -qx 'WLR_DRM_NO_ATOMIC=unset' <<<"$out" \
  && ok 'a physical GPU keeps its renderer and atomic modesetting' || bad "physical GPU lost its renderer: $out"

# --- an amdgpu desktop with a second, non-virtio card ---------------------
rm -rf "$work/drm"
card card0 amdgpu 1002:73BF
card card1 simple-framebuffer ''
out="$(run)"
grep -qx 'WLR_NO_HARDWARE_CURSORS=unset' <<<"$out" \
  && ok 'no virtio card, no software cursor' || bad "non-virtio machine forced a software cursor: $out"

# --- QEMU's virtio-gpu -----------------------------------------------------
rm -rf "$work/drm"
card card0 virtio_gpu 1AF4:1050
out="$(run)"
grep -qx 'WLR_NO_HARDWARE_CURSORS=1' <<<"$out" \
  && ok "virtio-gpu gets the software cursor its flipped hardware cursor needs" \
  || bad "virtio-gpu kept the broken hardware cursor: $out"
grep -qx 'WLR_RENDERER=pixman' <<<"$out" && grep -qx 'WLR_DRM_NO_ATOMIC=1' <<<"$out" \
  && ok 'virtio-gpu renders with pixman on legacy modesetting' || bad "virtio-gpu renderer: $out"

# --- the greeter's startup record --------------------------------------------
# tilo-boot-health reads it to tell a greeter that presented a frame from one
# that crash-loops while greetd stays active; both must name the same file.
rm -rf "$work/drm"
card card0 amdgpu 1002:73BF
out="$(run)"
grep -qx "TILO_STARTUP_METRICS_PATH=$work/run-greeter/startup-metrics.json" <<<"$out" \
  && ok "the greeter writes its startup record where the boot assessment reads it" \
  || bad "the greeter's startup record is not in its record directory: $out"
record="$(sed -n 's/^GREETER_RECORD="\${TILO_BOOT_HEALTH_GREETER_RECORD:-\(.*\)}"$/\1/p' \
  "$repo/os/usr/libexec/tilo-boot-health")"
default_dir="$(sed -n 's/^greeter_record_dir="\${TILO_GREETER_RECORD_DIR:-\(.*\)}"$/\1/p' "$wrapper")"
[ -n "$record" ] && [ "$record" = "$default_dir/startup-metrics.json" ] \
  && ok "tilo-boot-health reads the record tilo-cage-session asks for ($record)" \
  || bad "tilo-boot-health reads '${record:-?}', tilo-cage-session writes '${default_dir:-?}/startup-metrics.json'"
input="$(sed -n 's/^GREETER_INPUT="\${TILO_BOOT_HEALTH_GREETER_INPUT:-\(.*\)}"$/\1/p' \
  "$repo/os/usr/libexec/tilo-boot-health")"
grep -qx "TILO_GREETER_INPUT_RECORD=$work/run-greeter/input" <<<"$out" \
  && [ -n "$input" ] && [ "$input" = "$default_dir/input" ] \
  && ok "the greeter records its keyboard or touch screen where tilo-boot-health reads it ($input)" \
  || bad "tilo-boot-health reads '${input:-?}', the greeter was given: $out"
grep -Eq "^d +$default_dir +0755 +greetd +greetd " "$repo/os/usr/lib/tmpfiles.d/tilo.conf" \
  && ok "$default_dir exists at boot, writable by the greeter user alone" \
  || bad "tmpfiles.d/tilo.conf does not create $default_dir 0755 greetd:greetd"
out="$(run TILO_STARTUP_METRICS_PATH=/tmp/diagnostic.json)"
grep -qx 'TILO_STARTUP_METRICS_PATH=/tmp/diagnostic.json' <<<"$out" \
  && ok "an explicit startup record path is kept" || bad "the wrapper replaced an explicit record path: $out"
out="$(umask 0077; run)"
grep -qx 'umask=0022' <<<"$out" \
  && ok "the record stays readable to the health check under a strict session umask" \
  || bad "the greeter's record would be private to it: $out"
out="$(program=/usr/libexec/tilo/tilo-installer run_program)"
grep -qx 'TILO_STARTUP_METRICS_PATH=unset' <<<"$out" \
  && ok "the installer does not write the greeter's record" || bad "the installer was given the greeter's record: $out"
rmdir "$work/run-greeter"
out="$(run)"
grep -qx 'TILO_STARTUP_METRICS_PATH=unset' <<<"$out" \
  && ok "no record directory, no record" || bad "the greeter was pointed at a missing directory: $out"

# --- the console stays quiet, the journal keeps the output -----------------
# greetd hands the session the login VT as stdout and stderr; cage, libseat
# and the greeter printed onto the console before the first frame and their
# errors never reached the journal.
python3 -I -c 'import socket,sys; socket.socket(socket.AF_UNIX).bind(sys.argv[1])' "$work/journal.sock"
cat >"$work/bin/systemd-cat" <<'EOF'
#!/bin/sh
printf 'systemd-cat=%s\n' "$*" >"$JOURNAL_ARGS"
while [ "$1" != -- ]; do shift; done
shift
exec "$@" >>"$JOURNAL_OUT" 2>&1
EOF
chmod +x "$work/bin/systemd-cat"
journaled() {
  env -u TILO_CAGE_SESSION_KEEP_OUTPUT PATH="$work/bin:$PATH" TILO_DRM_SYSFS="$work/drm" \
    TILO_CAGE="$work/bin/cage" TILO_GREETER_RECORD_DIR="$work/run-greeter" \
    TILO_JOURNAL_STDOUT_SOCKET="$work/journal.sock" JOURNAL_ARGS="$work/journal.args" \
    JOURNAL_OUT="$work/journal.out" "$@"
}
: >"$work/journal.out"
console="$(journaled bash "$wrapper" /usr/libexec/tilo/tilo-greeter 2>&1)"
[ -z "$console" ] \
  && ok 'nothing the greeter session prints reaches the console' \
  || bad "the greeter session printed onto the console: $console"
grep -q -- '^systemd-cat=--identifier=tilo-greeter --stderr-priority=warning -- .*bash '"$wrapper"' /usr/libexec/tilo/tilo-greeter$' "$work/journal.args" \
  && ok 'the greeter session logs under tilo-greeter, stderr at warning' \
  || bad "unexpected journal routing: $(cat "$work/journal.args")"
grep -qx 'args=-s -- /usr/libexec/tilo/tilo-greeter' "$work/journal.out" \
  && grep -qx 'TILO_CAGE_SESSION_KEEP_OUTPUT=unset' "$work/journal.out" \
  && ok 'cage and the greeter run once, with their output in the journal' \
  || bad "cage did not run under the journal stream: $(cat "$work/journal.out")"
journaled bash "$wrapper" /usr/libexec/tilo/tilo-installer >/dev/null 2>&1
grep -q -- '--identifier=tilo-installer ' "$work/journal.args" \
  && ok 'the installer logs under its own name' \
  || bad "the installer is not journaled as tilo-installer: $(cat "$work/journal.args")"
# --- a greeter that cannot start leaves a text sign-in ---------------------
# A greeter that exited without starting a session (on the laptop this was
# reported on, its window was wider than any texture) made greetd exit;
# systemd restarted it into its start limit and the person was left at a
# console with no way to sign in. Now the same VT offers greetd's text
# greeter, starting the session the graphical one would.
cat >"$work/bin/agreety" <<'EOF'
#!/bin/sh
printf 'agreety=%s\n' "$*"
EOF
chmod +x "$work/bin/agreety"
mkdir -p "$work/run-greeter"
session_entry="$repo/os/usr/share/wayland-sessions/shell.desktop"
fallback() {
  : >"$work/tty"
  run TILO_TEXT_GREETER="$work/bin/agreety" TILO_GREETER_TTY="$work/tty" \
    TILO_SESSION_ENTRY="$session_entry" "$@" 2>"$work/fallback.err"
}
out="$(fallback CAGE_EXIT=1)"
status=$?
session_exec="$(sed -n 's/^Exec=//p' "$session_entry")"
[ "$status" -eq 0 ] \
  && grep -qx "agreety=--cmd env XDG_SESSION_TYPE=wayland XDG_SESSION_DESKTOP=tilo XDG_CURRENT_DESKTOP=tilo $session_exec" "$work/tty" \
  && ok 'a greeter that cannot start leaves the text sign-in for the same session' \
  || bad "no text sign-in after the greeter failed (status $status): $(cat "$work/tty" "$work/fallback.err")"
grep -q 'could not start, so you can sign in here instead' "$work/tty" \
  && ok 'the text sign-in says why the screen looks different' \
  || bad "the text sign-in has no explanation: $(cat "$work/tty")"
grep -q 'exited with status 1' "$work/fallback.err" \
  && ok "the greeter's failure is logged" || bad "the failure was not logged: $(cat "$work/fallback.err")"
out="$(fallback CAGE_EXIT=0)"
status=$?
[ "$status" -eq 0 ] && [ ! -s "$work/tty" ] \
  && ok 'a greeter that started the session leaves nothing behind' \
  || bad "the text sign-in ran after a successful sign-in (status $status): $(cat "$work/tty")"
: >"$work/tty"
out="$(program=/usr/libexec/tilo/tilo-installer run_program CAGE_EXIT=3 \
  TILO_TEXT_GREETER="$work/bin/agreety" TILO_GREETER_TTY="$work/tty" 2>/dev/null)"
status=$?
[ "$status" -eq 3 ] && [ ! -s "$work/tty" ] \
  && ok 'the installer keeps its own exit status, with no text sign-in' \
  || bad "the installer fell back to a sign-in (status $status): $(cat "$work/tty")"
out="$(run CAGE_EXIT=1 TILO_TEXT_GREETER="$work/bin/agreety" TILO_GREETER_TTY="$work/no-such-dir/tty" 2>/dev/null)"
status=$?
[ "$status" -eq 1 ] \
  && ok 'without a terminal the failure reaches greetd, whose recovery page takes over' \
  || bad "a greeter failure without a terminal exited $status"

grep -Fq 'command = "/usr/libexec/tilo-cage-session /usr/libexec/tilo/tilo-greeter"' "$repo/os/etc/greetd/config.toml" \
  && ok 'greetd starts the greeter through tilo-cage-session' \
  || bad 'greetd starts the greeter without tilo-cage-session, so its output reaches the console'

exit "$fail"
