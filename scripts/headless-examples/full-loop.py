#!/usr/bin/env python3
"""End-to-end flow in one file: spawn → drive → snapshot → tear down.

This is the most copy-pastable example. It starts a headless session of this
tree's build (`real_apps.ScriptedSession`), drives it, takes snapshots and
stops it. Use this as the template for a CI smoke test or an AI agent's
exploratory probe. A check that wants the tree's apps uses
`real_apps.Session` instead.

Usage:
  ./full-loop.py [SOCKET]

What it does:
  1. Spawns ./target/debug/tilo-session --backend headless on a scratch
     profile, with TILO_TEST_CONTROL set to the socket path.
  2. Lists targets, opens the strip switcher (Super+S), clicks its card,
     closes it (Escape).
  3. Opens global search (Super+Space), types "Files", proves its live state,
     waits for the transition to settle, and snapshots.
  4. Closes the palette (Escape), snapshots again.
  5. Stops the session and removes the profile, the socket and the runtime.

Total runtime: ~5 seconds on a warm cargo cache.

Exits non-zero on any refusal of the session's or a failed assertion.
"""
import os
import pathlib
import struct
import sys
import time

from real_apps import Failed, ScriptedSession

SCRIPT_DIR = pathlib.Path(__file__).resolve().parent
BINARY = pathlib.Path(
    os.environ.get(
        "TILO_SHELL_BINARY",
        SCRIPT_DIR / ".." / ".." / "target" / "debug" / "tilo-session",
    )
).resolve()
SOCKET = sys.argv[1] if len(sys.argv) > 1 else None
# A snapshot renders a whole frame, and the first one on a software adapter
# compiles every pipeline first: it is bounded as a hang, not as a reply.
SNAPSHOT_TIMEOUT = float(os.environ.get("TILO_TEST_HANG_BOUND_SECS") or 5.0)
PALETTE_PNG = pathlib.Path("/tmp/full-loop-palette.png")
FINAL_PNG = pathlib.Path("/tmp/full-loop-final.png")


def state(session) -> dict:
    response = session.control({"op": "state_dump"})
    if "state" not in response:
        raise RuntimeError(f"unexpected state_dump response: {response}")
    return response["state"]


def palette(session) -> dict:
    return state(session)["chrome"]["palette"]


def has_target(session, name: str) -> bool:
    """Whether the compositor offers `name` as a target with an area."""
    return any(
        t["name"] == name and t["rect"][2] > 0 and t["rect"][3] > 0
        for t in session.control({"op": "list_targets"})["targets"]
    )


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
    # 1. Spawn, on a scratch profile of its own.
    try:
        session = ScriptedSession(BINARY, socket=SOCKET).start()
    except Failed as error:
        print(f"FAIL: {error}", file=sys.stderr)
        return 1
    print(f"socket up: {session.control}")
    time.sleep(0.5)  # let the first paint complete
    try:
        # 2. List targets, open the strip switcher, click its card.
        resp = session.control({"op": "list_targets"})
        print(f"  {len(resp['targets'])} targets")
        session.control({"op": "key_down", "key": "Super"})
        session.control({"op": "key", "key": "s"})
        session.control({"op": "key_up", "key": "Super"})
        wait_until("strip switcher to open",
                   lambda: state(session)["chrome"].get("strip_switcher_open"))
        # The state flips at the key press; the open labels have a place to
        # click only once a frame has laid them out.
        wait_until("open strip switcher to be laid out",
                   lambda: has_target(session, "strip_switcher"))
        session.control({"op": "click_named", "name": "strip_switcher"})
        print("  clicked strip_switcher")
        session.control({"op": "key", "key": "Escape"})
        wait_until("strip switcher to close",
                   lambda: not state(session)["chrome"].get("strip_switcher_open"))

        # 3. Fresh profiles use Super+Space for global search, which opens
        # scoped to the workspace in front (its launcher's scope); typing
        # still reaches other workspaces, the scope only ranks what is here.
        # Hold the modifier through the production key-down/up path.
        session.control({"op": "key_down", "key": "Super"})
        session.control({"op": "key", "key": "Space"})
        session.control({"op": "key_up", "key": "Super"})
        wait_until("global-search palette to open", lambda: palette(session)["visible"])
        session.control({"op": "type", "text": "Files"})
        wait_until(
            "palette query to become Files",
            lambda: palette(session)["visible"] and palette(session)["query"] == "Files",
        )
        if state(session)["chrome"]["search_scope"] != "Workspace":
            raise RuntimeError("Super+Space did not open global search on the workspace")
        input_target = next(
            (t for t in session.control({"op": "list_targets"})["targets"]
             if t["name"] == "command_palette_input"), None,
        )
        if input_target is None:
            raise RuntimeError("palette is live in state but has no visible semantic target")
        time.sleep(0.5)  # palette transition is 400ms; do not attest a cross-fade frame
        session.control({"op": "snapshot", "path": str(PALETTE_PNG)},
                        timeout=SNAPSHOT_TIMEOUT)
        assert_nonblank_png(PALETTE_PNG)
        assert_palette_input_painted(PALETTE_PNG, input_target["rect"])
        print(f"  {PALETTE_PNG} written and verified")

        # 4. Escape clears what was typed, a second Escape closes search;
        # snapshot the final state.
        session.control({"op": "key", "key": "Escape"})
        wait_until("palette query to clear",
                   lambda: palette(session)["visible"] and palette(session)["query"] == "")
        session.control({"op": "key", "key": "Escape"})
        wait_until("palette to close", lambda: not palette(session)["visible"])
        if any(
            t["name"] == "command_palette_input"
            for t in session.control({"op": "list_targets"})["targets"]
        ):
            raise RuntimeError("closed palette still exports a visible semantic input target")
        time.sleep(0.5)  # let the close transition finish before judging pixels
        session.control({"op": "snapshot", "path": str(FINAL_PNG)},
                        timeout=SNAPSHOT_TIMEOUT)
        assert_nonblank_png(FINAL_PNG)
        assert_palette_is_visually_gone(PALETTE_PNG, FINAL_PNG)
        print(f"  {FINAL_PNG} written and verified")

        return 0
    finally:
        # 5. Tear down: the session, its profile, its socket and runtime.
        session.stop()


if __name__ == "__main__":
    sys.exit(main())
