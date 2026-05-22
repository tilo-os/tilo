#!/usr/bin/env python3
"""End-to-end flow in one file: spawn → drive → snapshot → tear down.

This is the most copy-pastable example. It DOES NOT assume a separate
spawn-headless.sh; it launches the compositor itself, drives it, takes
snapshots, and kills it. Use this as the template for a CI smoke test
or an AI agent's exploratory probe.

Usage:
  ./full-loop.py [/tmp/nirios-full-loop.sock]

What it does:
  1. Spawns ./target/debug/quick-desktop --backend headless with
     NIRIOS_TEST_CONTROL set to the socket path and DISPLAY +
     WAYLAND_DISPLAY unset.
  2. Waits up to 15s for the socket to appear.
  3. Lists targets, picks the workspace wheel, clicks it.
  4. Opens the palette (Ctrl+K), types "Files", snapshots.
  5. Closes the palette (Escape), snapshots again.
  6. Kills the compositor + removes the socket.

Total runtime: ~5 seconds on a warm cargo cache.

Exits non-zero on any compositor error response or process timeout.
"""
import os
import pathlib
import subprocess
import sys
import time

from _control import call, require_ok

SOCKET = sys.argv[1] if len(sys.argv) > 1 else "/tmp/nirios-full-loop.sock"
SCRIPT_DIR = pathlib.Path(__file__).resolve().parent
BINARY = SCRIPT_DIR / ".." / ".." / "target" / "debug" / "quick-desktop"
BINARY = BINARY.resolve()


def main() -> int:
    if not BINARY.exists():
        print(f"FAIL: compositor not built at {BINARY}", file=sys.stderr)
        print("      run: cargo build -p quick-desktop --bin quick-desktop", file=sys.stderr)
        return 1

    # 1. Spawn.
    try:
        os.unlink(SOCKET)
    except FileNotFoundError:
        pass
    env = {**os.environ, "NIRIOS_TEST_CONTROL": SOCKET}
    env.pop("DISPLAY", None)
    env.pop("WAYLAND_DISPLAY", None)
    print(f"spawning {BINARY} --backend headless ...")
    proc = subprocess.Popen(
        [str(BINARY), "--backend", "headless"],
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )

    try:
        # 2. Wait for socket.
        deadline = time.time() + 15.0
        while time.time() < deadline:
            if os.path.exists(SOCKET):
                break
            time.sleep(0.2)
        else:
            print("FAIL: socket did not appear within 15s", file=sys.stderr)
            return 1
        print(f"socket up: {SOCKET}")
        time.sleep(0.5)  # let the first paint complete

        # 3. List targets + click wheel_disc.
        resp = call(SOCKET, {"op": "list_targets"})
        print(f"  {len(resp['targets'])} targets")
        require_ok(call(SOCKET, {"op": "click_named", "name": "wheel_disc"}))
        print("  clicked wheel_disc")

        # 4. Ctrl+K → palette, type, snapshot.
        require_ok(call(SOCKET, {"op": "modifiers", "ctrl": True, "shift": False, "alt": False, "logo": False}))
        require_ok(call(SOCKET, {"op": "key", "key": "k"}))
        require_ok(call(SOCKET, {"op": "modifiers", "ctrl": False, "shift": False, "alt": False, "logo": False}))
        require_ok(call(SOCKET, {"op": "type", "text": "Files"}))
        require_ok(call(SOCKET, {"op": "snapshot", "path": "/tmp/full-loop-palette.png"}))
        print("  /tmp/full-loop-palette.png written")

        # 5. Escape, snapshot final state.
        require_ok(call(SOCKET, {"op": "key", "key": "Escape"}))
        require_ok(call(SOCKET, {"op": "snapshot", "path": "/tmp/full-loop-final.png"}))
        print("  /tmp/full-loop-final.png written")

        return 0
    finally:
        # 6. Tear down.
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
