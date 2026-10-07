#!/usr/bin/env python3
"""Open global search with Super+Space, type a query, snapshot.

Usage:
  ./keyboard-flow.py [SOCKET] [QUERY] [/tmp/out.png]

Demonstrates the modifier + key + type pattern. The palette opens on
Super+Space (the desktop-wide search shortcut); typing into it sets the live query, which
filters command results in the palette UI.

Pattern is generic — substitute any chord by changing the modifiers and
the key name. Valid named keys: Enter / Escape / Tab / ArrowLeft / Up /
Right / Down / Backspace / Delete / Home / End / PageUp / PageDown /
Space. Anything else is treated as a single character.
"""
import sys
from _control import call, require_ok, socket_path_from_argv

sock = socket_path_from_argv()
query = sys.argv[2] if len(sys.argv) > 2 else "terminal"
out = sys.argv[3] if len(sys.argv) > 3 else "/tmp/palette.png"

# Hold Super, send Space, release Super.
require_ok(call(sock, {"op": "modifiers", "ctrl": False, "shift": False, "alt": False, "logo": True}))
require_ok(call(sock, {"op": "key", "key": "Space"}))
require_ok(call(sock, {"op": "modifiers", "ctrl": False, "shift": False, "alt": False, "logo": False}))

# Type the query.
require_ok(call(sock, {"op": "type", "text": query}))

# Snapshot.
require_ok(call(sock, {"op": "snapshot", "path": out}))

print(f"done — palette with query {query!r} written to {out}")
