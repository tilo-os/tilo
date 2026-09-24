#!/usr/bin/env python3
"""Demonstrate the drag command — press at A, motion through `steps`
interpolated points, release at B. Snapshots before + after.

Usage:
  ./drag-demo.py [SOCKET]

The drag here moves through the middle of the screen. To drag a real
target (e.g. a tab handle), use list_targets to find its rect first,
then derive coordinates from it.
"""
from _control import call, require_ok, socket_path_from_argv

sock = socket_path_from_argv()

require_ok(call(sock, {"op": "snapshot", "path": "/tmp/drag-before.png"}))

require_ok(call(sock, {
    "op": "drag",
    "x_from": 400, "y_from": 300,
    "x_to":   900, "y_to":   500,
    "steps":  16,
}))

require_ok(call(sock, {"op": "snapshot", "path": "/tmp/drag-after.png"}))

print("done — /tmp/drag-before.png + /tmp/drag-after.png")
