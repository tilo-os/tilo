#!/usr/bin/env bash
# Diagnose a verified dev disk in a disposable guest overlay.
set -euo pipefail

config=/etc/greetd/config.toml
log=/tmp/tilo-cage-diagnostic.log
original=$(mktemp /tmp/tilo-greetd-config.XXXXXX)
cp -- "$config" "$original"
restore() {
  cp -- "$original" "$config"
  rm -f -- "$original"
  systemctl reset-failed greetd.service || true
  systemctl restart greetd.service || true
}
trap restore EXIT

variant=${GREETER_RENDER_VARIANT:-gpu}
case "$variant" in
  gpu|gpu-generic-cpu|vello-cpu|gl-softpipe|lp-zero|lp-no-rast) ;;
  *) echo "unsupported greeter render variant: $variant" >&2; exit 2 ;;
esac

cat >/tmp/tilo-child-status.sh <<'SH'
#!/usr/bin/env bash
TILO_STARTUP_METRICS_PATH="/tmp/tilo-greeter-metrics-$$.json" /usr/bin/tilo-greeter
status=$?
echo "TILO_GREETER_EXIT=$status"
exit "$status"
SH
if [[ $variant == vello-cpu ]]; then
  sed -i '2i export TILO_UI_VELLO_CPU=1' /tmp/tilo-child-status.sh
elif [[ $variant == gpu-generic-cpu ]]; then
  sed -i '2i export LD_PRELOAD=/tmp/libtilo-gallivm-cpu-interpose.so MESA_SHADER_CACHE_DISABLE=1' /tmp/tilo-child-status.sh
elif [[ $variant == gl-softpipe ]]; then
  sed -i '2i export WGPU_BACKEND=gl GALLIUM_DRIVER=softpipe LIBGL_ALWAYS_SOFTWARE=1' /tmp/tilo-child-status.sh
elif [[ $variant == lp-zero ]]; then
  sed -i '2i export LP_NUM_THREADS=0' /tmp/tilo-child-status.sh
elif [[ $variant == lp-no-rast ]]; then
  sed -i '2i export LP_NO_RAST=1' /tmp/tilo-child-status.sh
fi
if [[ ${GREETER_CAPTURE_FAULT_PC:-false} == true ]]; then
  sed -i 's#/usr/bin/tilo-greeter#/tmp/tilo-greeter-ptrace-probe /usr/bin/tilo-greeter#' /tmp/tilo-child-status.sh
fi
chmod 0755 /tmp/tilo-child-status.sh

echo '::group::guest display before greeter restart'
ls -l /dev/dri 2>&1 || true
lscpu | grep -E 'Architecture:|Model name:|Features:|Flags:' || true
systemctl status greetd.service --no-pager -l 2>&1 || true
echo '::endgroup::'

python3 - "$config" "$original" "$log" "$variant" <<'PY'
from pathlib import Path
import sys

path = Path(sys.argv[1])
text = Path(sys.argv[2]).read_text()
log = sys.argv[3]
variant = sys.argv[4]
commands = (
    'command = "env WLR_NO_HARDWARE_CURSORS=1 cage -s -- /usr/bin/tilo-greeter"',
    'command = "/usr/libexec/tilo-cage-session /usr/bin/tilo-greeter"',
)
launcher = (
    'WLR_LOG=debug /usr/libexec/tilo-cage-session'
    if variant == 'gpu'
    else 'WLR_NO_HARDWARE_CURSORS=1 WLR_RENDERER=pixman '
         'WLR_DRM_NO_ATOMIC=1 WLR_LOG=debug cage -s --'
)
instrumented = (
    'command = "/usr/bin/bash -c '
    "'" + launcher + ' '
    '/tmp/tilo-child-status.sh >>' + log + ' 2>&1\'"'
)
if sum(text.count(command) for command in commands) != 1:
    raise SystemExit('expected exactly one graphical greeter command')
for command in commands:
    text = text.replace(command, instrumented)
path.write_text(text)
PY

rm -f -- "$log" /tmp/tilo-greeter-metrics-*.json
systemctl reset-failed greetd.service
systemctl restart greetd.service
if [[ $variant == gpu || $variant == gpu-generic-cpu ]]; then
  # Keep this disposable overlay's instrumented greeter alive for the caller's
  # process and framebuffer checks. The production launcher includes the
  # hardware-specific CPU shim; baseline LLVM codegen can take longer.
  trap - EXIT
  for attempt in $(seq 1 40); do
    if grep -lq 'render-worker-first-present-done' /tmp/tilo-greeter-metrics-*.json 2>/dev/null; then
      break
    fi
    if grep -q 'TILO_GREETER_EXIT=' "$log" 2>/dev/null; then
      break
    fi
    sleep 1
  done
else
  sleep 6
fi
echo '::group::guest greeter diagnostic'
if [[ -s $log ]]; then
  grep -Ei 'error|failed|fatal|panic|signal|segfault|denied|permission|renderer|started|exited|TILO_GREETER_EXIT|TILO_SIGILL|TILO_PTRACE|illegal instruction' "$log" | tail -n 50 | cut -c 1-320 || true
  echo 'last 20 lines:'
  tail -n 20 "$log" | cut -c 1-320
else
  echo 'cage wrote no diagnostic log'
fi
for metrics in /tmp/tilo-greeter-metrics-*.json; do
  [[ -f $metrics ]] || continue
  echo "startup marks from $metrics:"
  jq -r '.events[]? | "\(.name) \(.ns)"' "$metrics" | tail -n 30 || true
done
echo '::group::guest SIGILL evidence'
printf 'core_pattern: '
cat /proc/sys/kernel/core_pattern 2>&1 || true
printf 'core_limit: '
ulimit -c || true
printf 'gdb: '
command -v gdb || true
printf 'recent kernel traps:\n'
journalctl -k -b --no-pager -o short-monotonic 2>&1 |
  grep -Ei 'tilo-greeter|undefined instruction|illegal instruction|sigill|trap|pc :' |
  tail -n 30 || true
if command -v coredumpctl >/dev/null 2>&1; then
  coredumpctl --no-pager --no-legend list /usr/bin/tilo-greeter 2>&1 | tail -n 15 || true
fi
echo '::endgroup::'
systemctl status greetd.service --no-pager -l 2>&1 || true
echo '::endgroup::'
