#!/usr/bin/env python3
"""Drive the command palette across its scope chips and snapshot the result.

Why this exists: `full-loop.py` opens the palette and types "Files", and the
snapshot it takes says *No results for "Files"* — while a tab literally named
Files is open two centimetres away. That reads as broken search. It isn't.

The palette opens scoped to the focused application ("Notes app"), and the
scope chips are drawn by the palette rather than registered as named targets,
so `click_named` cannot reach them. Clicking the "Everywhere" chip by
coordinate and re-querying returns what you expected all along:

    APPLICATIONS  1
      Open Files          System · so.tilo.file-explorer
    FILES        40
      ...

Two things worth knowing before you trust a screenshot from this:

1. **The default scope is a UX question, not a bug in search.** Ctrl+K on a
   fresh session finds nothing outside the focused app until you widen it.
   Whether that default is right is a design decision; it is currently the
   reason the palette looks empty in every automated capture.

2. **The results come from your real home directory.** With HOME pointed at a
   synthetic tree the file results go away *and so does the application
   result*, because the application index resolves through the same
   environment. So a publishable screenshot of this either needs a seeded
   fixture home that the app index also honours, or manual redaction. Do not
   ship the raw output: it contains your directory names.

Usage:
  ./palette-scope-demo.py [out.png] [socket]

Exits non-zero if the compositor errors or the socket never appears.
"""
import os
import pathlib
import subprocess
import sys
import time

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from _control import call, require_ok  # noqa: E402

OUT = sys.argv[1] if len(sys.argv) > 1 else "/tmp/palette-scope.png"
SOCKET = sys.argv[2] if len(sys.argv) > 2 else "/tmp/tilo-palette-scope.sock"
BINARY = (pathlib.Path(__file__).resolve().parent / ".." / ".." / "target" / "debug" / "shell").resolve()

# The "Everywhere" chip, in the default 1600x900 headless render. The chips are
# not addressable by name, so this is a coordinate. If the palette layout
# changes, re-derive it from a snapshot rather than nudging it blindly.
EVERYWHERE_CHIP = (1065, 170)
QUERY = "Files"


def main() -> int:
    if not BINARY.exists():
        print(f"FAIL: compositor not built at {BINARY}", file=sys.stderr)
        print("      run: cargo build -p shell --bin shell", file=sys.stderr)
        return 1

    try:
        os.unlink(SOCKET)
    except FileNotFoundError:
        pass

    env = {**os.environ, "TILO_TEST_CONTROL": SOCKET}
    env.pop("DISPLAY", None)
    env.pop("WAYLAND_DISPLAY", None)
    proc = subprocess.Popen(
        [str(BINARY), "--backend", "headless"],
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )

    try:
        deadline = time.time() + 15.0
        while time.time() < deadline and not os.path.exists(SOCKET):
            time.sleep(0.2)
        if not os.path.exists(SOCKET):
            print("FAIL: socket did not appear within 15s", file=sys.stderr)
            return 1
        time.sleep(0.6)  # first paint

        # Ctrl+K opens the palette, scoped to the focused application.
        require_ok(call(SOCKET, {"op": "modifiers", "ctrl": True, "shift": False, "alt": False, "logo": False}))
        require_ok(call(SOCKET, {"op": "key", "key": "k"}))
        require_ok(call(SOCKET, {"op": "modifiers", "ctrl": False, "shift": False, "alt": False, "logo": False}))
        time.sleep(0.3)

        x, y = EVERYWHERE_CHIP
        require_ok(call(SOCKET, {"op": "click_at", "x": x, "y": y}))
        time.sleep(0.3)

        require_ok(call(SOCKET, {"op": "type", "text": QUERY}))
        time.sleep(0.5)
        require_ok(call(SOCKET, {"op": "snapshot", "path": OUT}))
        print(f"wrote {OUT}")
        print("NOTE: check the file listing for private paths before sharing this image.")
        return 0
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
        try:
            os.unlink(SOCKET)
        except FileNotFoundError:
            pass


if __name__ == "__main__":
    sys.exit(main())
