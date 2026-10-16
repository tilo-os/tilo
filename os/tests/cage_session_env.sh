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
for name in WLR_RENDERER WLR_DRM_NO_ATOMIC WLR_NO_HARDWARE_CURSORS TILO_STARTUP_METRICS_PATH TILO_CAGE_SESSION_KEEP_OUTPUT; do
  eval "value=\${$name-unset}"
  printf '%s=%s\n' "$name" "$value"
done
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
  env -u WLR_RENDERER -u WLR_DRM_NO_ATOMIC -u WLR_NO_HARDWARE_CURSORS -u TILO_STARTUP_METRICS_PATH \
    -u TILO_CAGE_SESSION_KEEP_OUTPUT TILO_JOURNAL_STDOUT_SOCKET="$work/no-journal" \
    PATH="$work/bin:$PATH" TILO_DRM_SYSFS="$work/drm" TILO_CAGE="$work/bin/cage" \
    TILO_GREETER_RECORD_DIR="$work/run-greeter" "$@" bash "$wrapper" "$program"
}
run() { program=/usr/bin/tilo-greeter run_program "$@"; }

# --- a physical GPU ------------------------------------------------------
rm -rf "$work/drm"
card card1 msm_dpu ''
out="$(run)"
grep -qx 'args=-s -- /usr/bin/tilo-greeter' <<<"$out" \
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
out="$(program=/usr/bin/tilo-installer run_program)"
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
console="$(journaled bash "$wrapper" /usr/bin/tilo-greeter 2>&1)"
[ -z "$console" ] \
  && ok 'nothing the greeter session prints reaches the console' \
  || bad "the greeter session printed onto the console: $console"
grep -q -- '^systemd-cat=--identifier=tilo-greeter --stderr-priority=warning -- .*bash '"$wrapper"' /usr/bin/tilo-greeter$' "$work/journal.args" \
  && ok 'the greeter session logs under tilo-greeter, stderr at warning' \
  || bad "unexpected journal routing: $(cat "$work/journal.args")"
grep -qx 'args=-s -- /usr/bin/tilo-greeter' "$work/journal.out" \
  && grep -qx 'TILO_CAGE_SESSION_KEEP_OUTPUT=unset' "$work/journal.out" \
  && ok 'cage and the greeter run once, with their output in the journal' \
  || bad "cage did not run under the journal stream: $(cat "$work/journal.out")"
journaled bash "$wrapper" /usr/bin/tilo-installer >/dev/null 2>&1
grep -q -- '--identifier=tilo-installer ' "$work/journal.args" \
  && ok 'the installer logs under its own name' \
  || bad "the installer is not journaled as tilo-installer: $(cat "$work/journal.args")"
grep -Fq 'command = "/usr/libexec/tilo-cage-session /usr/bin/tilo-greeter"' "$repo/os/etc/greetd/config.toml" \
  && ok 'greetd starts the greeter through tilo-cage-session' \
  || bad 'greetd starts the greeter without tilo-cage-session, so its output reaches the console'

exit "$fail"
