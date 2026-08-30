#!/usr/bin/env bash
set -euo pipefail

for helper in qos-projector qos-doubletake qos-fluxcast; do
  test -x "/usr/libexec/$helper"
done

gst-inspect-1.0 openh264enc >/dev/null
python3 - <<'PY'
import dbus_next
import gi
import json
import subprocess

devices = subprocess.run(
    ["/usr/libexec/qos-projector", "discover", "--timeout-ms", "0"],
    check=True,
    capture_output=True,
    text=True,
    timeout=15,
).stdout
assert isinstance(json.loads(devices), list)
PY

# This is intentionally a live session-bus call. It proves the booted shell
# acquired its backend name and exported the ScreenCast interface, rather than
# merely checking that an XML file was copied into the image.
busctl --user introspect \
  org.freedesktop.impl.portal.desktop.qos \
  /org/freedesktop/portal/desktop \
  org.freedesktop.impl.portal.ScreenCast | grep -q 'CreateSession'
