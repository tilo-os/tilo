#!/usr/bin/env python3
"""A press lands on the row it appears to land on.

The claim this proves is the one a person actually makes of a file
manager: "I can see `photo.png`; if I press it, `photo.png` is what
responds." So nothing here asks the app where it thinks its rows are.
The targets are read out of the screenshot — the list is the one thing
on screen with a constant vertical pitch, so the longest run of evenly
spaced ink bands *is* the list — and the verdict is read out of the
next screenshot, from where the selection ended up.

That distinction is the whole point. A harness that aims with the
app's own hit-test geometry agrees with itself by construction and
photographs a working demo while the real thing misses, which is
exactly what happened here twice:

  * the compositor painted a client frame whose size lagged its
    configure *centred* in the tile while routing pointer input by the
    tile's origin, so every visible pixel sat half the size mismatch
    away from where a press addressed it; and
  * Files reserved the band for the shell's floating chip by asking
    the shell how wide it had been on the *previous* frame, so the
    list was built at one height and laid out at another, and jumped
    the first time anything rebuilt it.

Both are invisible to a test that aims in hit-test space and obvious
to this one.

Usage:
  ./press-lands-where-drawn-proof.py [--keep] [--grid]

Exits non-zero if any row fails to respond to a press at its own
painted position, or if the list moves between frames.
"""
from __future__ import annotations

import argparse
import json
import os
import pathlib
import socket
import subprocess
import sys
import tempfile
import time

from reaper import die_with_parent

try:
    from PIL import Image
except ImportError:  # pragma: no cover - environment guard
    print("this proof reads pixels; install Pillow (uv pip install pillow)")
    raise SystemExit(1)

REPO = pathlib.Path(__file__).resolve().parents[2]
SHELL = REPO / "target" / "debug" / "shell"
FILES = "so.tilo.file-explorer"

# `LIST_ROW_H` in apps/file-explorer/src/views/file_view.rs. Only used to
# recognise the list *as* a list in the screenshot — never to aim.
ROW_PITCH = 34.0
PITCH_SLACK = 3.0

# The files the session is given, in the order the list sorts them.
SAMPLE = ["alder.txt", "birch.txt", "cedar.txt", "elm.txt", "hazel.txt"]


def spawn(root: str, home: str) -> tuple[subprocess.Popen, str, str]:
    """A session with its own everything, but the machine's app registry.

    `TILO_APPS_DATA_DIR` / `TILO_RUNTIME_BUNDLE_DIR` are machine state that
    lives under `XDG_DATA_HOME`; repointing the latter to isolate the
    session's documents silently isolates them too, and every launch then
    fails with "no bundle was found".
    """
    sock = os.path.join(root, "c.sock")
    runtime = os.path.join(root, "run")
    os.makedirs(runtime, mode=0o700, exist_ok=True)
    machine = os.path.expanduser("~")
    env = {
        **os.environ,
        "TILO_TEST_CONTROL": sock,
        "HOME": home,
        "TILO_STATE_PATH": os.path.join(root, "config", "state.json"),
        "XDG_DATA_HOME": os.path.join(root, "data"),
        "XDG_CACHE_HOME": os.path.join(root, "cache"),
        "XDG_STATE_HOME": os.path.join(root, "state"),
        "XDG_CONFIG_HOME": os.path.join(root, "config"),
        "XDG_RUNTIME_DIR": runtime,
        "TILO_APPS_DATA_DIR": os.path.join(machine, ".local/share/tilo-apps"),
        "TILO_RUNTIME_BUNDLE_DIR": os.path.join(machine, ".local/share/tilo-runtime"),
        "TILO_PEER_KEY_PATH": os.path.join(root, "keys", "peer-key"),
        "TILO_HEADLESS_SIZE": "1600x1000",
        # German chrome would still pass, but the shots are meant to be read.
        "LC_ALL": "en_US.UTF-8",
        "LANG": "en_US.UTF-8",
        "RUST_LOG": "info",
    }
    # winit falls back to X11, so clearing only the wayland variable still
    # opens a window on the operator's screen.
    env.pop("DISPLAY", None)
    env.pop("WAYLAND_DISPLAY", None)
    log_path = os.path.join(root, "shell.log")
    log = open(log_path, "w")
    proc = subprocess.Popen(
        [str(SHELL), "--backend", "headless"],
        env=env,
        stdout=log,
        stderr=subprocess.STDOUT,
        preexec_fn=die_with_parent(),
    )
    deadline = time.time() + 90
    while time.time() < deadline and not os.path.exists(sock):
        if proc.poll() is not None:
            print(open(log_path).read()[-3000:])
            raise SystemExit("the compositor exited before it opened its socket")
        time.sleep(0.3)
    if not os.path.exists(sock):
        raise SystemExit("the compositor never opened its control socket")
    time.sleep(1.5)
    return proc, sock, log_path


def call(sock: str, payload: dict, timeout: float = 90.0) -> dict:
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(timeout)
    s.connect(sock)
    try:
        s.sendall((json.dumps(payload) + "\n").encode())
        buf = b""
        while not buf.endswith(b"\n"):
            chunk = s.recv(65536)
            if not chunk:
                break
            buf += chunk
    finally:
        s.close()
    return json.loads(buf.decode())


def files_tile(sock: str) -> dict:
    """Wait for Files to actually present a frame, and report its tile.

    `frame_seq > 0` is the ground truth: a tile can look like a window
    while still being the placeholder.
    """
    deadline = time.time() + 90
    while time.time() < deadline:
        frames = call(sock, {"op": "foreign_frames"})["foreign_frames"]["frames"]
        for f in frames:
            if f["app_id"] == FILES and f.get("frame_seq", 0) > 0 and f.get("rect"):
                time.sleep(4)  # let the first real layout settle
                return f["rect"]
        time.sleep(1)
    raise SystemExit("Files never presented a frame")


def ink_bands(path: str, box: tuple[float, float, float, float]) -> list[tuple[int, int]]:
    """Contiguous runs of rows carrying bright text ink inside `box`."""
    im = Image.open(path).convert("RGB")
    px = im.load()
    w, h = im.size
    x0, y0, x1, y1 = (int(max(0, box[0])), int(max(0, box[1])),
                      int(min(w, box[2])), int(min(h, box[3])))
    lit = [y for y in range(y0, y1)
           if sum(1 for x in range(x0, x1) if sum(px[x, y]) > 240) >= 3]
    if not lit:
        return []
    bands, start, prev = [], lit[0], lit[0]
    for y in lit[1:]:
        if y <= prev + 2:
            prev = y
            continue
        bands.append((start, prev))
        start = prev = y
    bands.append((start, prev))
    return bands


def painted_rows(path: str, box) -> list[tuple[int, int]]:
    """The file rows, picked out of the ink by their even spacing.

    Self-validating: a list that stopped being evenly spaced yields a
    short run and the proof fails, rather than quietly aiming elsewhere.
    """
    bands = ink_bands(path, box)
    centres = [(a + b) / 2.0 for a, b in bands]
    best: list[int] = []
    for i in range(len(centres)):
        run = [i]
        for j in range(i + 1, len(centres)):
            if abs(centres[j] - centres[run[-1]] - ROW_PITCH) <= PITCH_SLACK:
                run.append(j)
        if len(run) > len(best):
            best = run
    return [bands[i] for i in best]


def selection_band(path: str, box) -> tuple[int, int] | None:
    """The accent-blue selection pill, if the list is showing one."""
    im = Image.open(path).convert("RGB")
    px = im.load()
    w, h = im.size
    x0, y0, x1, y1 = (int(max(0, box[0])), int(max(0, box[1])),
                      int(min(w, box[2])), int(min(h, box[3])))
    span = max(1, x1 - x0)
    lit = []
    for y in range(y0, y1):
        n = sum(1 for x in range(x0, x1)
                if px[x, y][2] > 120 and px[x, y][2] - px[x, y][0] > 40)
        if n > span * 0.5:
            lit.append(y)
    if not lit:
        return None
    bands, start, prev = [], lit[0], lit[0]
    for y in lit[1:]:
        if y <= prev + 1:
            prev = y
            continue
        bands.append((start, prev))
        start = prev = y
    bands.append((start, prev))
    return max(bands, key=lambda b: b[1] - b[0])


def press(sock: str, x: float, y: float) -> None:
    call(sock, {"op": "pointer_move", "x": float(x), "y": float(y)})
    time.sleep(0.35)
    call(sock, {"op": "pointer_button", "pressed": True})
    time.sleep(0.3)
    call(sock, {"op": "pointer_button", "pressed": False})
    time.sleep(1.5)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--keep", action="store_true",
                    help="leave the session directory behind for inspection")
    ap.add_argument("--out", default=None, help="where to write the shots")
    args = ap.parse_args()

    if not SHELL.exists():
        print(f"{SHELL} is missing; cargo build -p shell --bin shell")
        return 1

    # A short root: an AF_UNIX path past ~108 bytes will not bind.
    root = tempfile.mkdtemp(prefix="/tmp/qs-")
    home = os.path.join(root, "home")
    os.makedirs(home, exist_ok=True)
    for name in SAMPLE:
        with open(os.path.join(home, name), "w") as fh:
            fh.write("x\n")
    out = args.out or os.path.join(root, "shots")
    os.makedirs(out, exist_ok=True)

    proc, sock, log_path = spawn(root, home)
    failures: list[str] = []
    try:
        call(sock, {"op": "actions_invoke", "provider": "applications", "id": FILES}, 120)
        tile = files_tile(sock)
        # The name column, in screen coordinates. Only bounds the search —
        # the row positions still come out of the pixels.
        names = (tile["x"] + tile["w"] * 0.24, tile["y"] + 60,
                 tile["x"] + tile["w"] * 0.62, tile["y"] + tile["h"])
        rows_pane = (tile["x"] + tile["w"] * 0.24, tile["y"] + 60,
                     tile["x"] + tile["w"] * 0.97, tile["y"] + tile["h"])

        first = os.path.join(out, "rows-as-painted.png")
        call(sock, {"op": "snapshot", "path": first})
        rows = painted_rows(first, names)
        print(f"the list is painted as {len(rows)} evenly spaced rows: "
              f"{[(a + b) // 2 for a, b in rows]}")
        if len(rows) != len(SAMPLE):
            print(f"expected {len(SAMPLE)} rows, read {len(rows)} out of the pixels")
            return 1
        anchor = [(a + b) // 2 for a, b in rows]

        for i in range(len(SAMPLE)):
            before = os.path.join(out, f"row{i}-before.png")
            call(sock, {"op": "snapshot", "path": before})
            now = painted_rows(before, names)
            if len(now) == len(rows):
                centres = [(a + b) // 2 for a, b in now]
                drift = max(abs(c - a) for c, a in zip(centres, anchor))
                if drift > 2:
                    failures.append(
                        f"the list moved {drift}px between frames "
                        f"({anchor} -> {centres}); a list that will not hold "
                        f"still cannot be aimed at"
                    )
                    anchor = centres

            target_y = (now or rows)[i]
            aim_y = (target_y[0] + target_y[1]) // 2
            aim_x = tile["x"] + tile["w"] * 0.35
            press(sock, aim_x, aim_y)

            after = os.path.join(out, f"row{i}-after.png")
            call(sock, {"op": "snapshot", "path": after})
            pill = selection_band(after, rows_pane)
            if pill is None:
                failures.append(
                    f"{SAMPLE[i]}: pressed its painted centre (y={aim_y}) and "
                    f"nothing was selected"
                )
                continue
            pill_mid = (pill[0] + pill[1]) // 2
            painted_after = painted_rows(after, names) or now or rows
            hit = min(range(len(painted_after)),
                      key=lambda j: abs(sum(painted_after[j]) // 2 - pill_mid))
            if hit != i:
                failures.append(
                    f"{SAMPLE[i]}: pressed its painted centre (y={aim_y}) and "
                    f"{SAMPLE[hit] if hit < len(SAMPLE) else '?'} responded "
                    f"(selection at y={pill[0]}..{pill[1]})"
                )
            else:
                print(f"{SAMPLE[i]}: pressed at its painted centre y={aim_y} -> selected")
    finally:
        proc.terminate()

    if failures:
        print("\nFAILED — a press did not land where it was aimed:")
        for f in failures:
            print(f"  - {f}")
        print(f"\nshots: {out}\nlog:   {log_path}")
        return 1

    print(f"\nevery row responded to a press at its own painted position")
    print(f"shots: {out}")
    if not args.keep:
        print("(pass --keep to hold on to the session directory)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
