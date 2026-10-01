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
  gpu|vello-cpu) ;;
  *) echo "unsupported greeter render variant: $variant" >&2; exit 2 ;;
esac

cat >/tmp/tilo-child-status.sh <<'SH'
#!/usr/bin/env bash
LD_PRELOAD=/tmp/tilo-sigill-probe.so \
  TILO_STARTUP_METRICS_PATH="/tmp/tilo-greeter-metrics-$$.json" /usr/bin/tilo-greeter
status=$?
echo "TILO_GREETER_EXIT=$status"
exit "$status"
SH
if [[ $variant == vello-cpu ]]; then
  sed -i '2i export TILO_UI_VELLO_CPU=1' /tmp/tilo-child-status.sh
fi
chmod 0755 /tmp/tilo-child-status.sh

echo '::group::guest display before greeter restart'
ls -l /dev/dri 2>&1 || true
ls -l /usr/bin/tilo-greeter /tmp/tilo-sigill-probe.so || true
getcap /usr/bin/tilo-greeter 2>&1 || true
LD_PRELOAD=/tmp/tilo-sigill-probe.so /usr/bin/true 2>&1 || true
lscpu | grep -E 'Architecture:|Model name:|Features:' || true
systemctl status greetd.service --no-pager -l 2>&1 || true
echo '::endgroup::'

python3 - "$config" "$original" "$log" <<'PY'
from pathlib import Path
import sys

path = Path(sys.argv[1])
text = Path(sys.argv[2]).read_text()
log = sys.argv[3]
commands = (
    'command = "env WLR_NO_HARDWARE_CURSORS=1 cage -s -- /usr/bin/tilo-greeter"',
    'command = "/usr/libexec/tilo-cage-session /usr/bin/tilo-greeter"',
)
instrumented = (
    'command = "/usr/bin/bash -c '
    "'WLR_NO_HARDWARE_CURSORS=1 WLR_RENDERER=pixman "
    'WLR_DRM_NO_ATOMIC=1 WLR_LOG=debug cage -s -- '
    '/tmp/tilo-child-status.sh >>' + log + ' 2>&1\'"'
)
if sum(text.count(command) for command in commands) != 1:
    raise SystemExit('expected exactly one graphical greeter command')
for command in commands:
    text = text.replace(command, instrumented)
path.write_text(text)
PY

rm -f -- "$log"
systemctl reset-failed greetd.service
systemctl restart greetd.service
sleep 6
echo '::group::guest greeter diagnostic'
if [[ -s $log ]]; then
  grep -Ei 'error|failed|fatal|panic|signal|segfault|denied|permission|renderer|started|exited|TILO_GREETER_EXIT|TILO_SIGILL|illegal instruction' "$log" | tail -n 50 | cut -c 1-320 || true
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
