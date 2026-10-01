#!/usr/bin/env bash
# Collect cage's stderr on a disposable dev guest without changing its image.
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

echo '::group::guest display before greeter restart'
ls -l /dev/dri 2>&1 || true
systemctl status greetd.service --no-pager -l 2>&1 || true
echo '::endgroup::'

python3 - "$config" <<'PY'
from pathlib import Path
import sys

path = Path(sys.argv[1])
original = 'command = "env WLR_NO_HARDWARE_CURSORS=1 cage -s -- /usr/bin/tilo-greeter"'
instrumented = (
    'command = "/usr/bin/bash -c '
    "'WLR_NO_HARDWARE_CURSORS=1 WLR_LOG=debug exec cage -s -- "
    '/usr/bin/tilo-greeter >>/tmp/tilo-cage-diagnostic.log 2>&1\'"'
)
text = path.read_text()
if text.count(original) != 1:
    raise SystemExit('expected exactly one stock graphical greeter command')
path.write_text(text.replace(original, instrumented))
PY

rm -f -- "$log"
systemctl reset-failed greetd.service
systemctl restart greetd.service
sleep 5
echo '::group::guest cage diagnostic'
if test -s "$log"; then
  tail -n 100 "$log" | cut -c 1-320
else
  echo 'cage wrote no diagnostic log'
fi
systemctl status greetd.service --no-pager -l 2>&1 || true
echo '::endgroup::'
