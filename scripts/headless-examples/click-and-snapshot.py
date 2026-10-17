#!/usr/bin/env python3
"""Click a named widget, then snapshot the post-click state to a PNG.

Usage:
  ./click-and-snapshot.py [SOCKET] [TARGET_NAME] [/tmp/out.png]

Defaults:
  socket = $TILO_TEST_CONTROL, else $XDG_RUNTIME_DIR/tilo-session/test-control.sock
  target = strip_switcher        (the strip switcher card; open it first with Super+S)
  output = /tmp/click-result.png

The target name comes from list_targets — every `Widget::test_id`-opted-in
widget gets a stable name like `strip_switcher` or `command_palette_input`;
others fall back to the path-based form `Parent/Child[index]`.
"""
import sys
from _control import call, require_ok, socket_path_from_argv

sock = socket_path_from_argv()
target = sys.argv[2] if len(sys.argv) > 2 else "strip_switcher"
out = sys.argv[3] if len(sys.argv) > 3 else "/tmp/click-result.png"

print(f"clicking {target!r} ...")
require_ok(call(sock, {"op": "click_named", "name": target}))

print(f"snapshotting to {out} ...")
require_ok(call(sock, {"op": "snapshot", "path": out}))

print(f"done — open {out} to inspect")
