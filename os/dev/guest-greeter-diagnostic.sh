#!/usr/bin/env bash
# Collect cage's stderr on a disposable dev guest without changing its image.
set -euo pipefail

config=/etc/greetd/config.toml
original=$(mktemp /tmp/tilo-greetd-config.XXXXXX)
cp -- "$config" "$original"
restore() {
  cp -- "$original" "$config"
  rm -f -- "$original"
  systemctl reset-failed greetd.service || true
  systemctl restart greetd.service || true
}
trap restore EXIT

echo '::group::guest display before greeter restart'
ls -l /dev/dri 2>&1 || true
systemctl status greetd.service --no-pager -l 2>&1 || true
echo '::endgroup::'

for variant in stock pixman legacy-drm pixman-legacy-drm; do
  log="/tmp/tilo-cage-${variant}.log"
  case "$variant" in
    stock) extra='';;
    pixman) extra='WLR_RENDERER=pixman ';;
    legacy-drm) extra='WLR_DRM_NO_ATOMIC=1 ';;
    pixman-legacy-drm) extra='WLR_RENDERER=pixman WLR_DRM_NO_ATOMIC=1 ';;
  esac
  python3 - "$config" "$original" "$log" "$extra" <<'PY'
from pathlib import Path
import sys

path = Path(sys.argv[1])
original_path = Path(sys.argv[2])
log = sys.argv[3]
extra = sys.argv[4]
original = 'command = "env WLR_NO_HARDWARE_CURSORS=1 cage -s -- /usr/bin/tilo-greeter"'
instrumented = (
    'command = "/usr/bin/bash -c '
    "'WLR_NO_HARDWARE_CURSORS=1 WLR_LOG=debug " + extra +
    'exec cage -s -- /usr/bin/tilo-greeter >>' + log + ' 2>&1\'"'
)
text = original_path.read_text()
if text.count(original) != 1:
    raise SystemExit('expected exactly one stock graphical greeter command')
path.write_text(text.replace(original, instrumented))
PY

  rm -f -- "$log"
  systemctl reset-failed greetd.service
  systemctl restart greetd.service
  sleep 4
  echo "::group::guest cage diagnostic: $variant"
  if test -s "$log"; then
    echo "log lines: $(wc -l < "$log")"
    grep -Ei 'error|failed|fatal|panic|signal|segfault|denied|permission|modesetting|renderer|started|exited' "$log" | tail -n 50 | cut -c 1-320 || true
    echo 'last 20 lines:'
    tail -n 20 "$log" | cut -c 1-320
  else
    echo 'cage wrote no diagnostic log'
  fi
  if systemctl is-active --quiet greetd && pgrep -x cage && pgrep -x tilo-greeter; then
    echo "variant $variant: GREETER_ALIVE"
  else
    echo "variant $variant: GREETER_EXITED"
  fi
  systemctl status greetd.service --no-pager -l 2>&1 || true
  echo '::endgroup::'
done

echo '::group::guest crash and kernel diagnostics'
journalctl -b -u greetd.service --no-pager -n 60 2>&1 || true
journalctl -b -k --no-pager 2>&1 | grep -Ei 'segfault|oom|killed process|virtio.gpu|drm|avc.*denied' | tail -n 60 || true
coredumpctl --no-pager list 2>&1 | tail -n 20 || true
echo '::endgroup::'
