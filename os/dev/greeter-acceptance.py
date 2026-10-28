#!/usr/bin/env python3
"""Real-boot acceptance of the sign-in screen, from outside the guest.

Drives a booted VM's greeter the way a person does, through QEMU's monitor
(absolute pointer, keyboard), and judges what is actually scanned out by
reading the display back with `screendump`. Every frame it captures must be
whole: no black holes where the screen was never painted, and nothing that
changes except what the input explains. It checks, in order:

  idle         the greeter before any input
  pointer      the pointer travelling over empty backdrop: only the pointer's
               current neighbourhood may change; no trail of earlier pointer
               positions, nothing black or stale
  typing       each passcode keystroke reaches the screen within the latency
               bound and repaints only a small part of it
  wrong        a wrong passcode: the greeter's console reports the error the
               person sees, and the error frame is whole and steady

The greeter's console (`installer-console.py`) says what state the screen is
in; the pixels say whether it was drawn. Screenshots and a JSON report land in
the output directory.

  greeter-acceptance.py --monitor MON.sock --console CONSOLE.sock --out DIR
                        [--width W --height H]
  greeter-acceptance.py --check-image FILE.ppm [--png FILE.png]
                                                 judge one capture
  greeter-acceptance.py --convert FILE.ppm --png FILE.png
                                                 keep one capture as PNG

Exit status: 0 accepted, 1 a check failed (reason on stderr), 2 unreachable.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
import socket
import statistics
import struct
import sys
import time
import zlib
from pathlib import Path

TILE = 16
# A tile whose every pixel is this dark was never painted: the greeter's
# darkest theme is a tinted near-black, not RGB zero.
BLACK = 3
# Pixel noise allowed between two captures of an unchanged region.
TOLERANCE = 6
# The pointer sprite and its anti-aliased edge, in pixels around the hotspot.
POINTER_BOX = 48
# A keystroke may repaint at most this fraction of the screen.
KEY_DAMAGE_FRACTION = 0.05
# Keystroke-to-screen bounds, in seconds, measured from QEMU accepting the
# key to the first capture that shows it. Captures themselves take time, so
# this is an upper bound on what the person waits.
KEY_LATENCY_MEDIAN = float(os.environ.get("TILO_GREETER_KEY_LATENCY_MEDIAN", "0.35"))
KEY_LATENCY_MAX = float(os.environ.get("TILO_GREETER_KEY_LATENCY_MAX", "0.8"))


class Failed(Exception):
    pass


# --- images -------------------------------------------------------------


class Image:
    def __init__(self, width: int, height: int, rgb: bytes):
        self.width = width
        self.height = height
        self.rgb = rgb

    @classmethod
    def read_ppm(cls, path: Path) -> "Image":
        data = path.read_bytes()
        fields: list[bytes] = []
        offset = 0
        while len(fields) < 4:
            while data[offset : offset + 1].isspace():
                offset += 1
            if data[offset : offset + 1] == b"#":
                offset = data.index(b"\n", offset) + 1
                continue
            end = offset
            while not data[end : end + 1].isspace():
                end += 1
            fields.append(data[offset:end])
            offset = end
        offset += 1
        if fields[0] != b"P6" or fields[3] != b"255":
            raise Failed(f"{path} is not an 8-bit binary PPM")
        width, height = int(fields[1]), int(fields[2])
        rgb = data[offset : offset + width * height * 3]
        if len(rgb) != width * height * 3:
            raise Failed(f"{path} is truncated")
        return cls(width, height, rgb)

    def write_png(self, path: Path) -> None:
        raw = b"".join(
            b"\x00" + self.rgb[y * self.width * 3 : (y + 1) * self.width * 3]
            for y in range(self.height)
        )

        def chunk(kind: bytes, body: bytes) -> bytes:
            return (
                struct.pack(">I", len(body))
                + kind
                + body
                + struct.pack(">I", zlib.crc32(kind + body) & 0xFFFFFFFF)
            )

        path.write_bytes(
            b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", self.width, self.height, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw, 6))
            + chunk(b"IEND", b"")
        )

    def tiles(self):
        for ty in range(0, self.height, TILE):
            for tx in range(0, self.width, TILE):
                yield tx, ty

    def tile_rows(self, tx: int, ty: int):
        x1 = min(tx + TILE, self.width)
        for y in range(ty, min(ty + TILE, self.height)):
            start = (y * self.width + tx) * 3
            yield self.rgb[start : start + (x1 - tx) * 3]

    def black_tiles(self) -> list[tuple[int, int]]:
        return [
            (tx, ty)
            for tx, ty in self.tiles()
            if all(max(row) <= BLACK for row in self.tile_rows(tx, ty))
        ]

    def changed_tiles(self, other: "Image") -> list[tuple[int, int]]:
        if (self.width, self.height) != (other.width, other.height):
            raise Failed("the display changed size between captures")
        changed = []
        for tx, ty in self.tiles():
            for a, b in zip(self.tile_rows(tx, ty), other.tile_rows(tx, ty)):
                if a != b and max(abs(p - q) for p, q in zip(a, b)) > TOLERANCE:
                    changed.append((tx, ty))
                    break
        return changed

    def distinct_colours(self, limit: int = 64) -> int:
        seen = set()
        step = max(1, (self.width * self.height) // 4096) * 3
        for i in range(0, len(self.rgb) - 2, step):
            seen.add(self.rgb[i : i + 3])
            if len(seen) >= limit:
                break
        return len(seen)


def judge_whole(image: Image, what: str) -> None:
    """A frame the greeter drew covers the whole display."""
    black = image.black_tiles()
    if black:
        xs = [x for x, _ in black]
        ys = [y for _, y in black]
        raise Failed(
            f"{what}: {len(black)} unpainted {TILE}px tiles, "
            f"spanning x {min(xs)}..{max(xs) + TILE}, y {min(ys)}..{max(ys) + TILE}"
        )
    if image.distinct_colours() < 8:
        raise Failed(f"{what}: the display is a flat colour, not the sign-in screen")


# --- the VM -------------------------------------------------------------


class Monitor:
    """QEMU's human monitor over its Unix socket, one persistent session."""

    def __init__(self, path: str):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            self.sock.connect(path)
        except OSError as error:
            raise ConnectionError(f"QEMU monitor {path}: {error}") from error
        self.sock.settimeout(30)
        self._read_prompt()

    def _read_prompt(self) -> str:
        data = b""
        while not data.endswith(b"(qemu) "):
            chunk = self.sock.recv(65536)
            if not chunk:
                raise ConnectionError("QEMU monitor closed")
            data += chunk
        return data.decode(errors="replace")

    def command(self, line: str) -> str:
        self.sock.sendall(line.encode() + b"\n")
        return self._read_prompt()

    def screendump(self, path: Path) -> Image:
        path.unlink(missing_ok=True)
        reply = self.command(f"screendump {path}")
        if "no surface" in reply.lower():
            raise Failed(
                "QEMU has no 2D surface to capture: the greeter acceptance needs "
                "the 2D display (vm.sh boot --2d)"
            )
        for _ in range(200):
            if path.exists() and path.stat().st_size > 0:
                try:
                    return Image.read_ppm(path)
                except (Failed, ValueError, IndexError):
                    pass
            time.sleep(0.01)
        raise Failed(f"screendump produced no image: {reply.strip()}")

    def pointer(self, x: int, y: int, width: int, height: int) -> None:
        qx = x * 32767 // (width - 1)
        qy = y * 32767 // (height - 1)
        self.command(f"mouse_move {qx} {qy} 0")

    def key(self, name: str) -> None:
        self.command(f"sendkey {name} 20")


def load_console():
    path = Path(__file__).with_name("installer-console.py")
    spec = importlib.util.spec_from_file_location("installer_console", path)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


def greeter_state(console, path: str) -> dict:
    return console.state(path)


def wait_for(predicate, timeout: float, what: str, interval: float = 0.2):
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        last = predicate()
        if last:
            return last
        time.sleep(interval)
    raise Failed(f"timed out after {timeout:.0f}s waiting for {what}")


# --- the checks ---------------------------------------------------------


class Run:
    def __init__(self, args):
        self.out = Path(args.out)
        self.out.mkdir(parents=True, exist_ok=True)
        self.width, self.height = args.width, args.height
        self.monitor = Monitor(args.monitor)
        self.console = load_console()
        self.console_path = args.console
        self.report: dict = {"display": [self.width, self.height]}
        self.captures = 0

    def capture(self, name: str | None = None) -> Image:
        self.captures += 1
        image = self.monitor.screendump(self.out / f".capture-{self.captures % 2}.ppm")
        if name:
            image.write_png(self.out / f"{name}.png")
        return image

    def state(self) -> dict:
        return greeter_state(self.console, self.console_path)

    def settle(self, seconds: float = 1.0) -> Image:
        """Wait until two captures a beat apart agree outside the caret."""
        previous = self.capture()
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            time.sleep(seconds)
            current = self.capture()
            if len(current.changed_tiles(previous)) <= 4:
                return current
            previous = current
        raise Failed("the sign-in screen never stopped changing on its own")

    def idle(self) -> Image:
        state = self.state()
        if state.get("phase") != "sign-in":
            raise Failed(f"the greeter is not offering sign-in: {state}")
        self.report["account"] = state.get("account")
        self.report["session_choice"] = state.get("session_choice")
        # Park the pointer in the top-left corner so it is not over a control.
        self.monitor.pointer(4, 4, self.width, self.height)
        image = self.settle()
        image.write_png(self.out / "01-idle.png")
        judge_whole(image, "idle")
        # A blinking caret is the only thing allowed to change on its own.
        changing = set()
        for _ in range(4):
            time.sleep(0.3)
            changing |= set(self.capture().changed_tiles(image))
        self.report["idle_changing_tiles"] = len(changing)
        if len(changing) > 8:
            raise Failed(f"idle: {len(changing)} tiles change with no input (a moving backdrop?)")
        self.blink = changing
        return image

    def pointer(self, reference: Image) -> None:
        w, h = self.width, self.height
        # Empty backdrop: the left and right margins, away from the centred
        # panel and the footer along the bottom.
        path = [
            (w // 12, h // 6),
            (w // 12, h // 2),
            (w // 8, h * 2 // 3),
            (w - w // 12, h // 6),
            (w - w // 10, h // 2),
            (w // 12, h // 6),
        ]
        previous = (4, 4)
        worst = 0
        for i, (x, y) in enumerate(path):
            self.monitor.pointer(x, y, w, h)
            time.sleep(0.35)
            image = self.capture(f"02-pointer-{i}" if i in (0, 3) else None)
            judge_whole(image, f"pointer at {x},{y}")
            # Only the pointer where it is now may differ. Where it was must
            # be restored: a stamp left behind is a trail of old cursors,
            # which a software cursor composited without honouring buffer
            # age leaves on screen.
            explained = [(x - POINTER_BOX, y - POINTER_BOX, x + POINTER_BOX, y + POINTER_BOX)]
            stray = [
                (tx, ty)
                for tx, ty in image.changed_tiles(reference)
                if (tx, ty) not in self.blink
                and not any(
                    tx + TILE > x0 and tx < x1 and ty + TILE > y0 and ty < y1
                    for x0, y0, x1, y1 in explained
                )
            ]
            worst = max(worst, len(stray))
            if stray:
                image.write_png(self.out / f"02-pointer-{i}-stale.png")
                raise Failed(
                    f"pointer at {x},{y}: {len(stray)} tiles away from the pointer "
                    f"differ from the idle frame, first at {stray[0]} (a pointer trail "
                    f"or stale region; the previous position was {previous})"
                )
            previous = (x, y)
        self.monitor.pointer(4, 4, w, h)
        time.sleep(0.3)
        back = self.capture("02-pointer-home")
        judge_whole(back, "pointer home")
        stray = [t for t in back.changed_tiles(reference) if t not in self.blink]
        if len(stray) > 8:
            raise Failed(f"after the pointer returned, {len(stray)} tiles stayed different")
        self.report["pointer_stray_tiles"] = worst

    def typing(self) -> Image:
        before = self.settle(0.5)
        latencies = []
        damage = []
        word = "wrongpass"
        for i, char in enumerate(word):
            reference = self.capture()
            sent = time.monotonic()
            self.monitor.key(char)
            while True:
                image = self.capture()
                changed = [t for t in image.changed_tiles(reference) if t not in self.blink]
                if changed:
                    latencies.append(time.monotonic() - sent)
                    break
                if time.monotonic() - sent > 5:
                    raise Failed(f"keystroke {i + 1} never reached the screen")
            # Let the frame finish, then measure what that keystroke repainted.
            time.sleep(0.15)
            image = self.capture("03-typing" if i == len(word) - 1 else None)
            judge_whole(image, f"after keystroke {i + 1}")
            changed = image.changed_tiles(reference)
            damage.append(len(changed) * TILE * TILE / (self.width * self.height))
        self.report["key_latency_s"] = [round(v, 3) for v in latencies]
        self.report["key_damage_fraction"] = [round(v, 4) for v in damage]
        median = statistics.median(latencies)
        self.report["key_latency_median_s"] = round(median, 3)
        if median > KEY_LATENCY_MEDIAN or max(latencies) > KEY_LATENCY_MAX:
            raise Failed(
                f"typing is slow: median {median * 1000:.0f} ms, worst "
                f"{max(latencies) * 1000:.0f} ms (bounds {KEY_LATENCY_MEDIAN * 1000:.0f} / "
                f"{KEY_LATENCY_MAX * 1000:.0f} ms)"
            )
        if max(damage) > KEY_DAMAGE_FRACTION:
            raise Failed(
                f"a keystroke repainted {max(damage) * 100:.1f}% of the screen "
                f"(bound {KEY_DAMAGE_FRACTION * 100:.0f}%)"
            )
        return before

    def wrong_passcode(self) -> None:
        self.monitor.key("ret")
        state = wait_for(
            lambda: (lambda s: s if (s.get("notice") or {}).get("kind") == "error" else None)(
                self.state()
            ),
            60,
            "the greeter to report the refused passcode",
            0.25,
        )
        notice = state["notice"]
        self.report["wrong_passcode_notice"] = notice
        self.report["wrong_passcode_status"] = state.get("status")
        expected = "greeter-error-passcode" if state.get("account") else "greeter-error-credentials"
        if notice.get("key") != expected:
            raise Failed(f"a wrong passcode reported {notice}, expected {expected}")
        status = state.get("status") or ""
        if not status or any(raw in status for raw in ("pam_", "AUTH_ERR", "greetd", ":")):
            raise Failed(f"the error is not a plain sentence: {status!r}")
        if state.get("phase") != "sign-in":
            raise Failed(f"after a wrong passcode the form is not ready again: {state}")
        image = self.settle(0.5)
        image.write_png(self.out / "04-wrong-passcode.png")
        judge_whole(image, "wrong passcode")
        # The error frame holds still: no flicker between captures.
        changes = []
        for _ in range(6):
            time.sleep(0.2)
            changes.append(len([t for t in self.capture().changed_tiles(image) if t not in self.blink]))
        self.report["error_frame_changes"] = changes
        if max(changes) > 8:
            raise Failed(f"the error frame keeps changing on its own: {changes}")


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--monitor")
    parser.add_argument("--console")
    parser.add_argument("--out")
    parser.add_argument("--width", type=int, default=1280)
    parser.add_argument("--height", type=int, default=800)
    parser.add_argument("--check-image")
    parser.add_argument("--convert")
    parser.add_argument("--png")
    args = parser.parse_args(argv)
    if args.convert:
        if not args.png:
            parser.error("--convert needs --png")
        Image.read_ppm(Path(args.convert)).write_png(Path(args.png))
        Path(args.convert).unlink()
        return 0
    if args.check_image:
        try:
            image = Image.read_ppm(Path(args.check_image))
            if args.png:
                image.write_png(Path(args.png))
                Path(args.check_image).unlink(missing_ok=True)
            judge_whole(image, args.check_image)
        except Failed as error:
            print(error, file=sys.stderr)
            return 1
        return 0
    if not (args.monitor and args.console and args.out):
        parser.error("--monitor, --console and --out are required")
    try:
        run = Run(args)
    except ConnectionError as error:
        print(error, file=sys.stderr)
        return 2
    try:
        reference = run.idle()
        run.pointer(reference)
        run.typing()
        run.wrong_passcode()
    except Failed as error:
        run.report["failure"] = str(error)
        print(f"greeter acceptance: {error}", file=sys.stderr)
        return 1
    finally:
        for stale in run.out.glob(".capture-*.ppm"):
            stale.unlink(missing_ok=True)
        (run.out / "greeter-acceptance.json").write_text(json.dumps(run.report, indent=2) + "\n")
        print(json.dumps(run.report, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
