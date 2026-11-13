#!/usr/bin/env python3
"""Demonstrate the drag command — press at A, motion through `steps`
interpolated points, release at B. Snapshots before + after.

Usage:
  ./drag-demo.py [SOCKET]

The drag here moves through the middle of the screen. To drag a real
target (e.g. a tab handle), use list_targets to find its rect first,
then derive coordinates from it.
"""
from real_apps import Session, control_socket_from_argv

session = Session.attach(control_socket_from_argv())

session.control({"op": "snapshot", "path": "/tmp/drag-before.png"})

session.control({
    "op": "drag",
    "x_from": 400, "y_from": 300,
    "x_to":   900, "y_to":   500,
    "steps":  16,
})

session.control({"op": "snapshot", "path": "/tmp/drag-after.png"})

print("done — /tmp/drag-before.png + /tmp/drag-after.png")
