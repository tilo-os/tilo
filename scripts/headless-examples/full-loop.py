#!/usr/bin/env python3
"""End-to-end flow in one file: spawn → drive → snapshot → tear down.

This is the most copy-pastable example. It DOES NOT assume a separate
spawn-headless.sh; it launches the compositor itself, drives it, takes
snapshots, and kills it. Use this as the template for a CI smoke test
or an AI agent's exploratory probe.

Usage:
  ./full-loop.py [SOCKET]   (default $XDG_RUNTIME_DIR/tilo-session/full-loop.sock)

What it does:
  1. Spawns ./target/debug/shell --backend headless with
     HOME and the XDG dirs in a scratch profile, TILO_TEST_CONTROL
     set to the socket path, and DISPLAY + WAYLAND_DISPLAY unset.
  2. Waits up to 15s for the socket to appear.
  3. Lists targets, picks the workspace wheel, clicks it.
  4. Opens global search (Super+Space), types "Files", proves its live state,
     waits for the transition to settle, and snapshots.
  5. Closes the palette (Escape), snapshots again.
  6. Kills the compositor + removes the socket.

Total runtime: ~5 seconds on a warm cargo cache.

Exits non-zero on any compositor error response or process timeout.
"""
import os
import pathlib
import shutil
import struct
import subprocess
import sys
import time

from _control import call, require_ok, runtime_socket_path
from tilo_client import scratch_session_env

SOCKET = sys.argv[1] if len(sys.argv) > 1 else runtime_socket_path("full-loop.sock")
SCRIPT_DIR = pathlib.Path(__file__).resolve().parent
BINARY = pathlib.Path(
    os.environ.get(
        "TILO_SHELL_BINARY",
        SCRIPT_DIR / ".." / ".." / "target" / "debug" / "shell",
    )
).resolve()
PALETTE_PNG = pathlib.Path("/tmp/full-loop-palette.png")
FINAL_PNG = pathlib.Path("/tmp/full-loop-final.png")


def state() -> dict:
    response = call(SOCKET, {"op": "state_dump"})
    if "state" not in response:
        raise RuntimeError(f"unexpected state_dump response: {response}")
    return response["state"]


def palette() -> dict:
    return state()["chrome"]["palette"]


def wait_until(description: str, predicate, timeout: float = 3.0) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.05)
    raise RuntimeError(f"timed out waiting for {description}")


def png_pixels(path: pathlib.Path) -> tuple[int, int, int, bytes]:
    """Reject a missing, malformed, transparent, or one-colour screenshot.

    This deliberately uses only the stdlib.  A PNG's compressed IDAT bytes
    can be non-empty even when every rendered pixel is identical, so inspect
    the decoded scanlines after undoing PNG filters.
    """
    import zlib

    data = path.read_bytes()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise RuntimeError(f"{path} is not a PNG")
    pos = 8
    width = height = colour_type = bit_depth = None
    compressed = bytearray()
    while pos + 12 <= len(data):
        length = struct.unpack(">I", data[pos:pos + 4])[0]
        kind = data[pos + 4:pos + 8]
        payload = data[pos + 8:pos + 8 + length]
        pos += 12 + length
        if kind == b"IHDR":
            width, height, bit_depth, colour_type = struct.unpack(">IIBB", payload[:10])
        elif kind == b"IDAT":
            compressed.extend(payload)
        elif kind == b"IEND":
            break
    channels = {0: 1, 2: 3, 4: 2, 6: 4}.get(colour_type)
    if not width or not height or bit_depth != 8 or channels is None:
        raise RuntimeError(f"{path} has unsupported or empty PNG geometry")
    raw = zlib.decompress(bytes(compressed))
    stride = width * channels
    previous = bytearray(stride)
    pixels = bytearray()
    offset = 0
    for _ in range(height):
        filter_kind = raw[offset]
        source = raw[offset + 1:offset + 1 + stride]
        offset += stride + 1
        row = bytearray(stride)
        for i, value in enumerate(source):
            left = row[i - channels] if i >= channels else 0
            above = previous[i]
            upper_left = previous[i - channels] if i >= channels else 0
            if filter_kind == 0:
                decoded = value
            elif filter_kind == 1:
                decoded = value + left
            elif filter_kind == 2:
                decoded = value + above
            elif filter_kind == 3:
                decoded = value + ((left + above) // 2)
            elif filter_kind == 4:
                p = left + above - upper_left
                pa, pb, pc = abs(p - left), abs(p - above), abs(p - upper_left)
                decoded = value + (left if pa <= pb and pa <= pc else above if pb <= pc else upper_left)
            else:
                raise RuntimeError(f"{path} uses invalid PNG filter {filter_kind}")
            row[i] = decoded & 0xff
        pixels.extend(row)
        previous = row
    return width, height, channels, bytes(pixels)


def assert_nonblank_png(path: pathlib.Path) -> None:
    width, height, channels, pixels = png_pixels(path)
    colours: set[bytes] = set()
    for i in range(0, len(pixels), channels):
        pixel = pixels[i:i + channels]
        if channels in (2, 4) and pixel[-1] == 0:
            continue
        colours.add(pixel)
        if len(colours) >= 8:
            return
    raise RuntimeError(f"{path} is blank or effectively one-colour ({len(colours)} colours)")


def assert_palette_input_painted(path: pathlib.Path, rect: list[float]) -> None:
    """Reject a scrim-only frame even when state and semantic targets are open."""
    width, height, channels, pixels = png_pixels(path)
    x, y, w, h = rect
    if not (0 <= x < x + w <= width and 0 <= y < y + h <= height):
        raise RuntimeError("search input target lies outside the captured output")
    # Inspect the typed query area, excluding the border and leading icon.
    # This example types Files at the default scale in a fresh profile.
    x0, x1 = int(x + 32), int(min(x + w - 8, x + 160))
    y0, y1 = int(y + 8), int(y + h - 8)
    luminances = []
    for py in range(y0, y1):
        for px in range(x0, x1):
            offset = (py * width + px) * channels
            rgb = pixels[offset:offset + min(channels, 3)]
            if len(rgb) < 3:
                value = rgb[0]
            else:
                value = (2126 * rgb[0] + 7152 * rgb[1] + 722 * rgb[2]) // 10000
            if channels in (2, 4):
                value = value * pixels[offset + channels - 1] // 255
            luminances.append(value)
    if not luminances:
        raise RuntimeError("search input has no captured interior")
    low, high = min(luminances), max(luminances)
    if (high - low < 80
            or sum(value <= low + 20 for value in luminances) < 80
            or sum(value >= low + 80 for value in luminances) < 80):
        raise RuntimeError("search state is open but its input text is not visibly painted")


def assert_palette_is_visually_gone(open_path: pathlib.Path, closed_path: pathlib.Path) -> None:
    """Prove Escape changed the rendered palette area, not only live state."""
    ow, oh, oc, open_pixels = png_pixels(open_path)
    cw, ch, cc, closed_pixels = png_pixels(closed_path)
    if (ow, oh, oc) != (cw, ch, cc):
        raise RuntimeError("open and closed screenshots use different pixel formats")
    changed = sum(a != b for a, b in zip(open_pixels, closed_pixels, strict=True))
    # The palette and scrim cover hundreds of thousands of channels at 1600x900.
    # A cursor blink or antialiasing wobble changes only a tiny fraction; require
    # a material frame change so stale retained palette pixels cannot pass.
    minimum = max(10_000, (ow * oh * oc) // 100)
    if changed < minimum:
        raise RuntimeError(
            f"palette state closed but rendered frame stayed stale: "
            f"only {changed} of {len(open_pixels)} channels changed (need {minimum})"
        )


def main() -> int:
    if not BINARY.exists():
        print(f"FAIL: compositor not built at {BINARY}", file=sys.stderr)
        print("      run: cargo build -p shell --bin shell", file=sys.stderr)
        return 1

    # 1. Spawn.
    try:
        os.unlink(SOCKET)
    except FileNotFoundError:
        pass
    env, profile = scratch_session_env()
    env["TILO_TEST_CONTROL"] = SOCKET
    env.pop("DISPLAY", None)
    env.pop("WAYLAND_DISPLAY", None)
    print(f"spawning {BINARY} --backend headless ...")
    try:
        proc = subprocess.Popen(
            [str(BINARY), "--backend", "headless"],
            env=env,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
    except Exception:
        shutil.rmtree(profile, ignore_errors=True)
        raise

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

        # 4. Fresh profiles use Super+Space for machine-wide search.
        # Hold the modifier through the production key-down/up path.
        require_ok(call(SOCKET, {"op": "key_down", "key": "Super"}))
        require_ok(call(SOCKET, {"op": "key", "key": "Space"}))
        require_ok(call(SOCKET, {"op": "key_up", "key": "Super"}))
        wait_until("global-search palette to open", lambda: palette()["visible"])
        require_ok(call(SOCKET, {"op": "type", "text": "Files"}))
        wait_until(
            "palette query to become Files",
            lambda: palette()["visible"] and palette()["query"] == "Files",
        )
        if state()["chrome"]["search_scope"] != "All":
            raise RuntimeError("Super+Space did not select machine-wide search scope")
        input_target = next(
            (t for t in call(SOCKET, {"op": "list_targets"})["targets"]
             if t["name"] == "command_palette_input"), None,
        )
        if input_target is None:
            raise RuntimeError("palette is live in state but has no visible semantic target")
        time.sleep(0.5)  # palette transition is 400ms; do not attest a cross-fade frame
        require_ok(call(SOCKET, {"op": "snapshot", "path": str(PALETTE_PNG)}))
        assert_nonblank_png(PALETTE_PNG)
        assert_palette_input_painted(PALETTE_PNG, input_target["rect"])
        print(f"  {PALETTE_PNG} written and verified")

        # 5. Escape, snapshot final state.
        require_ok(call(SOCKET, {"op": "key", "key": "Escape"}))
        wait_until("palette to close", lambda: not palette()["visible"])
        if any(
            t["name"] == "command_palette_input"
            for t in call(SOCKET, {"op": "list_targets"})["targets"]
        ):
            raise RuntimeError("closed palette still exports a visible semantic input target")
        time.sleep(0.5)  # let the close transition finish before judging pixels
        require_ok(call(SOCKET, {"op": "snapshot", "path": str(FINAL_PNG)}))
        assert_nonblank_png(FINAL_PNG)
        assert_palette_is_visually_gone(PALETTE_PNG, FINAL_PNG)
        print(f"  {FINAL_PNG} written and verified")

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
        shutil.rmtree(profile, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
