#!/usr/bin/env python3
"""Open an app this machine does not have, from the search box, on camera.

The beat: the operator opens the command palette, types the name of an
application that is not installed, presses Enter, and the app opens as a
tab and draws its own UI. No install button, no progress dialog, no
second step.

This script reproduces that end to end, headless, and *checks* it — it
is not merely a screenshot generator. Every claim below is read off the
compositor's own state over the test-control socket, and the run exits
non-zero the moment one of them is false:

  1. The registry offers Notes and the machine does not have it.
  2. The palette row for it is byte-identical to an installed app's:
     same "Open Notes" title, same subtitle shape, no "install",
     "download", "fetch" or "available" anywhere in it.
  3. Enter — a real keystroke, not a click on the row — activates it.
  4. Notes becomes a `TileContent::Foreign` tab and presents frames of
     its own (`frame_seq > 0`), so the picture is the Notes client rather
     than the compositor's first-frame placeholder.

Three screenshots land in `--out`: `palette.png` (the row), `pending.png`
(the one moment the machine admits the app is coming from somewhere) and
`tab.png` (Notes visibly running). Pass `--record PATH` to capture the same
verified run as a browser-ready H.264 MP4. The recorder samples the live
headless compositor; it does not replay a `shots` fixture or assemble a
facsimile out of unrelated screenshots.

    scripts/headless-examples/install-on-open.py --out target/install-on-open \\
        --record target/install-on-open/notes-open.mp4

On the day, run it in the mode that sets the scene and gets out of the
way — registry up, Notes absent, a real window to point a camera at —
and type it yourself:

    scripts/headless-examples/install-on-open.py --backend winit --stage-only

Everything it needs is built by:

    cargo build -p shell -p notes -p install -p launcher
    cargo build --release -p notes
    cargo build -p app-cli --bins --examples

## What is real and what is staged

Real: the `.tapp` is packed and signed by `pkg`, served over a live HTTP
socket, fetched by `app`, verified fail-closed and unpacked by
`tilo-install`, then started by `tilo-launcher` inside bwrap. The compositor
is a test-enabled build of the shipping compositor/layout/paint code on its
headless backend; release builds omit the control socket, not those paint
passes.

Staged: the registry is a loopback server on this machine rather than a
hosted one, and `HOME` is the synthetic tree `scripts/headless-examples/isolated-home.py`
builds so the palette shows a small fixture corpus instead of the operator's own
files. Neither changes a code path — they change what the code path is
pointed at.

`XDG_DATA_DIRS` is deliberately left alone: the Vulkan loader finds its
ICD manifests through it, and a staged-only value leaves wgpu with no
adapter and nothing on screen.
"""
from __future__ import annotations

import argparse
import json
import os
import pathlib
import re
import shutil
import signal
import socket
import subprocess
import sys
import time

import importlib.util


def _load_isolated_home():
    """`isolated-home.py`, by path.

    The filename has a hyphen in it, so it is not importable as a
    module name however the sys.path is arranged.
    """
    # Loading it by path otherwise drops a `scripts/__pycache__/` in the
    # working tree on every run.
    sys.dont_write_bytecode = True
    path = pathlib.Path(__file__).resolve().parent / "isolated-home.py"
    spec = importlib.util.spec_from_file_location("isolated_home", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


stage_home = _load_isolated_home().stage

PACKAGE_ID = "so.tilo.notes"
DISPLAY_NAME = "Notes"
# What the operator types. It is sent as individual text events on camera,
# rather than appearing as an already-complete fixture query.
QUERY = "Notes"

# Words that must never reach a row for an app the machine has to fetch.
# The row says "open", because opening is what the user asked for.
FORBIDDEN_IN_ROW = ("install", "download", "fetch", "available")

# Scrubbed from the compositor's environment so a run measures the
# binaries rather than whatever the invoking shell carried. The display
# vars matter most: a compositor must not find an outer session to
# attach to.
SCRUB = (
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "VK_DRIVER_FILES",
    "VK_ICD_FILENAMES",
    "__EGL_VENDOR_LIBRARY_FILENAMES",
    "MESA_LOADER_DRIVER_OVERRIDE",
    "DRI_PRIME",
    "__GLX_VENDOR_LIBRARY_NAME",
)

ANSI = re.compile(r"\x1b\[[0-9;]*m")


def parse_size(value: str) -> tuple[int, int]:
    match = re.fullmatch(r"\s*([1-9][0-9]*)[xX]([1-9][0-9]*)\s*", value)
    if match is None:
        raise argparse.ArgumentTypeError("size must be WIDTHxHEIGHT, for example 1600x900")
    width, height = (int(match.group(1)), int(match.group(2)))
    if width > 16384 or height > 16384:
        raise argparse.ArgumentTypeError("size dimensions must not exceed 16384")
    return width, height


def png_size(path: pathlib.Path) -> tuple[int, int]:
    """Read a PNG's IHDR dimensions without adding an image dependency."""
    header = path.read_bytes()[:24]
    if len(header) != 24 or header[:8] != b"\x89PNG\r\n\x1a\n" or header[12:16] != b"IHDR":
        raise RuntimeError(f"not a valid PNG: {path}")
    return int.from_bytes(header[16:20], "big"), int.from_bytes(header[20:24], "big")


def video_size(path: pathlib.Path) -> tuple[int, int]:
    ffprobe = shutil.which("ffprobe")
    if ffprobe is None:
        raise RuntimeError("validating --record needs ffprobe in PATH")
    output = subprocess.run(
        [
            ffprobe, "-v", "error", "-select_streams", "v:0",
            "-show_entries", "stream=width,height", "-of", "csv=p=0:s=x",
            str(path),
        ],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    try:
        return parse_size(output)
    except argparse.ArgumentTypeError as error:
        raise RuntimeError(f"ffprobe returned invalid dimensions {output!r}") from error


# ---------------------------------------------------------------------
# test-control socket
# ---------------------------------------------------------------------

def call(sock_path: str, payload: dict, timeout: float = 30.0) -> dict:
    """One command, one reply, on a fresh connection."""
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(timeout)
    s.connect(sock_path)
    try:
        s.sendall(json.dumps(payload).encode() + b"\n")
        buf = b""
        while b"\n" not in buf:
            chunk = s.recv(1 << 20)
            if not chunk:
                raise RuntimeError("compositor closed the connection mid-reply")
            buf += chunk
        return json.loads(buf.partition(b"\n")[0])
    finally:
        s.close()


def require_ok(resp: dict) -> dict:
    if "err" in resp:
        raise RuntimeError(f"compositor returned err: {resp['err']}")
    return resp.get("ok", resp)


def _walk(node, visit) -> None:
    if isinstance(node, dict):
        visit(node)
        for v in node.values():
            _walk(v, visit)
    elif isinstance(node, list):
        for v in node:
            _walk(v, visit)


def foreign_tiles(state) -> set:
    """`(app_id, surface_id)` for every adopted client toplevel.

    A `TileContent::Foreign` entry in the live tree is the compositor's
    own record that it took a client's xdg_toplevel into a tab. Nothing
    in the process table proves that.
    """
    found = set()

    def visit(d):
        f = d.get("Foreign")
        if isinstance(f, dict) and "app_id" in f:
            found.add((f["app_id"], f.get("surface_id")))

    _walk(state, visit)
    return found


# ---------------------------------------------------------------------
# the local signed registry
# ---------------------------------------------------------------------

class Registry:
    """`serve-test-registry`, packing a real binary into a real `.tapp`.

    The example's own default payload is `/bin/true`, which installs
    fine and exits immediately — it proves the fetch and never proves
    the open. Pointing it at the Notes binary is what makes step 4
    possible at all.
    """

    def __init__(self, example_bin: pathlib.Path, payload: pathlib.Path,
                 config_dir: pathlib.Path, logpath: pathlib.Path):
        self.example_bin = example_bin
        self.payload = payload
        self.config_dir = config_dir
        self.logpath = logpath
        self.proc = None
        self.log = None
        self.base_url = None

    def __enter__(self):
        self.config_dir.mkdir(parents=True, exist_ok=True)
        self.log = self.logpath.open("w+b")
        self.proc = subprocess.Popen(
            [str(self.example_bin),
             "--config-dir", str(self.config_dir),
             "--payload", str(self.payload),
             "--package-id", PACKAGE_ID,
             "--name", DISPLAY_NAME],
            stdout=self.log, stderr=subprocess.STDOUT,
            preexec_fn=os.setsid,
        )
        # It prints "registry http://127.0.0.1:PORT serving ..." and
        # flushes as soon as it is listening. Packing a ~100 MB debug
        # binary happens before that line, so the wait is real.
        deadline = time.time() + 120.0
        while time.time() < deadline:
            if self.proc.poll() is not None:
                raise SystemExit(
                    f"the test registry exited early ({self.proc.returncode}); "
                    f"see {self.logpath}")
            text = self.logpath.read_text(errors="replace")
            m = re.search(r"registry (http://\S+) serving", text)
            if m:
                self.base_url = m.group(1)
                return self
            time.sleep(0.25)
        raise SystemExit(f"the test registry never announced a port; see {self.logpath}")

    def __exit__(self, *exc):
        if self.proc is not None:
            try:
                os.killpg(os.getpgid(self.proc.pid), signal.SIGTERM)
                self.proc.wait(timeout=10)
            except (ProcessLookupError, subprocess.TimeoutExpired):
                try:
                    os.killpg(os.getpgid(self.proc.pid), signal.SIGKILL)
                except ProcessLookupError:
                    pass
        if self.log is not None:
            self.log.close()
        return False


# ---------------------------------------------------------------------
# the compositor
# ---------------------------------------------------------------------

class Compositor:
    def __init__(self, binaries, env_extra, logpath, startup_timeout, backend):
        self.sock = f"/tmp/tilo-install-on-open-{os.getpid()}.sock"
        self.binaries = binaries
        self.backend = backend
        self.env_extra = env_extra
        self.logpath = logpath
        self.startup_timeout = startup_timeout
        self.proc = None
        self.log = None

    def __enter__(self):
        try:
            os.unlink(self.sock)
        except FileNotFoundError:
            pass
        env = {**os.environ, "TILO_TEST_CONTROL": self.sock, "RUST_LOG": "info"}
        for k in SCRUB:
            env.pop(k, None)
        env.update(self.env_extra)
        self.log = self.logpath.open("wb")
        self.proc = subprocess.Popen(
            [str(self.binaries["shell"]), "--backend", self.backend],
            env=env, stdout=self.log, stderr=subprocess.STDOUT,
            preexec_fn=os.setsid,
        )
        deadline = time.time() + self.startup_timeout
        while time.time() < deadline:
            if os.path.exists(self.sock):
                time.sleep(1.0)  # let the first paint land
                return self
            if self.proc.poll() is not None:
                raise SystemExit(
                    f"the compositor exited with status {self.proc.returncode} "
                    f"before binding its control socket; see {self.logpath}")
            time.sleep(0.1)
        raise SystemExit(
            f"the compositor never bound {self.sock} within "
            f"{self.startup_timeout:.0f}s; see {self.logpath}")

    def __exit__(self, *exc):
        if self.proc is not None:
            try:
                os.killpg(os.getpgid(self.proc.pid), signal.SIGTERM)
                self.proc.wait(timeout=12)
            except (ProcessLookupError, subprocess.TimeoutExpired):
                try:
                    os.killpg(os.getpgid(self.proc.pid), signal.SIGKILL)
                except ProcessLookupError:
                    pass
        if self.log is not None:
            self.log.close()
        try:
            os.unlink(self.sock)
        except FileNotFoundError:
            pass
        return False

    # -- driving -------------------------------------------------------

    def modifiers(self, *, ctrl=False, shift=False, alt=False, logo=False):
        require_ok(call(self.sock, {"op": "modifiers", "ctrl": ctrl, "shift": shift,
                                    "alt": alt, "logo": logo}))

    def key(self, name: str):
        # Winit's names, not xkb's: "Enter", not "Return". The socket
        # rejects an unknown multi-character key rather than typing it,
        # so a wrong name here is a `parse_error`, not a silent no-op.
        require_ok(call(self.sock, {"op": "key", "key": name}))

    def type_text(self, text: str):
        require_ok(call(self.sock, {"op": "type", "text": text}))

    def snapshot(self, path: pathlib.Path):
        require_ok(call(self.sock, {"op": "snapshot", "path": str(path)}))

    def state(self) -> dict:
        reply = call(self.sock, {"op": "state_dump"})
        if "err" in reply:
            raise RuntimeError(f"state_dump: {reply['err']}")
        return reply.get("state", reply)

    def palette(self) -> dict:
        return self.state().get("chrome", {}).get("palette", {})

    def notifications(self) -> list:
        return self.state().get("chrome", {}).get("notifications", [])

    def foreign_frames(self):
        reply = call(self.sock, {"op": "foreign_frames"})
        body = reply.get("foreign_frames", reply.get("ok", reply))
        if isinstance(body, dict):
            body = body.get("frames", body)
        return body if isinstance(body, list) else []

    def open_palette(self, chord: str):
        """Open the command palette with one of its four chords.

        The chord matters, and it is the one thing about this beat that
        is easy to get wrong on camera.

        `Super+Space` and `Alt+Space` open the palette in
        global scope — "search the machine" — which is the only scope
        that offers to open an app you do not already have. `Ctrl+K`
        asks the narrower question, "search inside what I am looking
        at", and is scoped to the focused app.

        `Ctrl+K` is not simply broken for this: with nothing focused,
        the scope widens to global and the row appears anyway. That is
        exactly why it is the wrong chord to film — it works on an
        empty workspace and stops working the moment there is a tab in
        front of you. `--chord ctrl-k` exists so that difference can be
        measured rather than argued about.

        `Alt+Space` is the one to reach for when Tilo is running as a
        Wayland client inside another desktop: the host swallows Super
        before the compositor sees it.
        """
        if chord == "ctrl-k":
            self.modifiers(ctrl=True)
            self.key("k")
        elif chord == "super-space":
            self.modifiers(logo=True)
            self.key("Space")
        elif chord == "alt-space":
            self.modifiers(alt=True)
            self.key("Space")
        else:
            raise ValueError(f"unknown chord {chord!r}")
        self.modifiers()


# ---------------------------------------------------------------------
# live compositor recorder
# ---------------------------------------------------------------------

class FrameRecorder:
    """Turn verified, event-driven compositor frames into a short MP4.

    This deliberately drives the compositor's live ``snapshot`` operation
    rather than borrowing a pre-written ``shots`` scenario.  A snapshot is a
    fresh paint of the same FrameBuilder that presents the desktop, including
    adopted Wayland client buffers, so the last frames can only contain Notes
    after the real package has been installed, launched and presented.

    Captures are event-driven, not a polling screen recorder.  A 1600x900
    snapshot is itself a GPU paint/readback/PNG encode; continuously requesting
    one starves the Wayland client whose startup this run is meant to show.
    Instead the driver captures the real state at each meaningful beat and
    gives that beat a deliberate on-screen duration.  Package installation and
    client startup run unobserved between ``pending`` and ``notes-final``; no
    unrelated frames or fabricated UI are substituted for them.
    """

    def __init__(self, compositor: Compositor, frames_dir: pathlib.Path):
        self.compositor = compositor
        self.frames_dir = frames_dir
        self.samples: list[tuple[pathlib.Path, float, str]] = []

    def start(self) -> None:
        if self.frames_dir.exists():
            shutil.rmtree(self.frames_dir)
        self.frames_dir.mkdir(parents=True)

    def capture(self, label: str, duration: float,
                *, mirror: pathlib.Path | None = None) -> pathlib.Path:
        if duration <= 0:
            raise ValueError("recorded beat duration must be positive")
        index = len(self.samples)
        safe_label = re.sub(r"[^a-z0-9-]+", "-", label.lower()).strip("-")
        path = self.frames_dir / f"frame-{index:02d}-{safe_label}.png"
        self.compositor.snapshot(path)
        if not path.exists() or path.stat().st_size == 0:
            raise RuntimeError(f"snapshot did not produce {path}")
        self.samples.append((path, duration, label))
        if mirror is not None:
            mirror.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(path, mirror)
        return path

    def stop(self) -> None:
        if len(self.samples) < 2:
            raise RuntimeError(
                f"snapshot recorder produced only {len(self.samples)} frame(s)")

    def encode(self, output: pathlib.Path, *, output_fps: int = 30) -> None:
        """Encode the deliberately timed event frames."""
        ffmpeg = shutil.which("ffmpeg")
        if ffmpeg is None:
            raise RuntimeError("--record needs ffmpeg in PATH")
        output.parent.mkdir(parents=True, exist_ok=True)
        timeline = self.frames_dir / "timeline.ffconcat"
        lines = ["ffconcat version 1.0"]
        for path, duration, _label in self.samples:
            lines.extend((f"file '{path.name}'", f"duration {duration:.6f}"))
        # concat ignores the final duration unless the final file is repeated.
        lines.append(f"file '{self.samples[-1][0].name}'")
        timeline.write_text("\n".join(lines) + "\n", encoding="utf-8")

        subprocess.run(
            [
                ffmpeg, "-y", "-hide_banner", "-loglevel", "warning",
                "-f", "concat", "-safe", "0", "-i", timeline.name,
                "-vf", f"fps={output_fps},format=yuv420p",
                "-an", "-c:v", "libx264", "-preset", "medium", "-crf", "18",
                "-movflags", "+faststart", str(output.resolve()),
            ],
            cwd=self.frames_dir,
            check=True,
        )


# ---------------------------------------------------------------------
# checks
# ---------------------------------------------------------------------

class Failure(Exception):
    pass


def check(condition: bool, message: str) -> None:
    if not condition:
        raise Failure(message)


def find_row(pal: dict) -> dict | None:
    """The row that would open our package, if the palette is offering one.

    Palette entry ids are positional (`result:0`, `result:1`, …) — the
    row's own identity lives in its subtitle, which is where the
    Applications provider puts the package id. Matching on the subtitle
    is therefore matching on what the user can actually read.
    """
    for row in pal.get("rows", []):
        if PACKAGE_ID in (row.get("description") or ""):
            return row
    return None


def wait_for_row(comp: Compositor, timeout: float) -> tuple[dict, dict]:
    """Wait until the palette offers our package, and return (palette, row).

    The catalogue answers the palette from an in-memory cache and never
    blocks a keystroke on HTTP, so the first query after a cold start
    legitimately has nothing in it and schedules a fetch. Re-querying is
    what a person does anyway — they keep typing.
    """
    deadline = time.time() + timeout
    last = []
    while time.time() < deadline:
        pal = comp.palette()
        last = pal.get("rows", [])
        row = find_row(pal)
        if row is not None:
            return pal, row
        # Nudge the query so the providers re-run against a now-warm
        # cache: one character back, one character forward.
        comp.key("Backspace")
        time.sleep(0.3)
        comp.type_text(QUERY[-1])
        time.sleep(0.7)
    raise Failure(
        f"no palette row for {PACKAGE_ID} within {timeout:.0f}s; "
        f"last rows were {last}")


def check_row_wording(row: dict) -> None:
    """The row must read as opening an app, not as acquiring one."""
    label = row.get("label", "")
    detail = row.get("description") or ""
    check(label == f"Open {DISPLAY_NAME}",
          f"the row must say 'Open {DISPLAY_NAME}', not {label!r}")
    visible = f"{label} {detail}".lower()
    for word in FORBIDDEN_IN_ROW:
        check(word not in visible,
              f"{word!r} must not appear in a row the user reads as opening "
              f"an app: {label!r} / {detail!r}")
    # The subtitle's shape is the tell a badge would break: an installed
    # app's is "<where it opens> · <category> · <package id>".
    if detail:
        check(PACKAGE_ID in detail,
              f"the subtitle must name the package it opens: {detail!r}")


def wait_for_tab(comp: Compositor, timeout: float) -> list:
    deadline = time.time() + timeout
    while time.time() < deadline:
        tiles = foreign_tiles(comp.state())
        hits = sorted(f"{a}#{s}" for a, s in tiles if PACKAGE_ID in str(a))
        if hits:
            return hits
        time.sleep(1.0)
    return []


def wait_for_first_frame(comp: Compositor, timeout: float) -> dict | None:
    """Hold until the app has actually presented a frame.

    Adoption and painting are different events. Between them the tab
    shows the compositor's "waiting for first frame" placeholder, which
    for an app that legitimately starts empty is hard to tell from the
    real thing by eye. `frame_seq` is the compositor's own count of
    buffers taken from the client, so `> 0` is the fact rather than the
    impression. A debug build's first vello pipeline compile is tens of
    seconds, which is what the timeout is sized for.
    """
    deadline = time.time() + timeout
    best = None
    while time.time() < deadline:
        for f in comp.foreign_frames():
            if PACKAGE_ID not in str(f.get("app_id", "")):
                continue
            best = f
            if (f.get("frame_seq") or 0) > 0:
                return f
        time.sleep(2.0)
    return best


def check_frame_in_viewport(frame: dict, viewport: tuple[int, int]) -> None:
    """Refuse a proof whose client exists but is clipped out of the picture."""
    rect = frame.get("rect") or {}
    try:
        x = float(rect["x"])
        y = float(rect["y"])
        width = float(rect["w"])
        height = float(rect["h"])
    except (KeyError, TypeError, ValueError) as error:
        raise Failure(f"Notes frame has no usable compositor rect: {frame}") from error
    view_width, view_height = viewport
    check(width > 0 and height > 0, f"Notes frame has an empty rect: {rect}")
    tolerance = 0.5
    check(
        x >= -tolerance
        and y >= -tolerance
        and x + width <= view_width + tolerance
        and y + height <= view_height + tolerance,
        f"Notes rect ({x:.1f}, {y:.1f}, {width:.1f}, {height:.1f}) is not fully "
        f"inside the {view_width}x{view_height} capture viewport",
    )


def app_log_lines(logpath: pathlib.Path, package_id: str) -> list[str]:
    text = ANSI.sub("", logpath.read_text(errors="replace"))
    tag = f"[app {package_id}] "
    return [l.split(tag, 1)[-1] for l in text.splitlines() if tag in l]


# ---------------------------------------------------------------------

def main() -> int:
    here = pathlib.Path(__file__).resolve().parents[2]
    p = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--worktree", type=pathlib.Path, default=here,
                   help="repo root holding target/debug (default: this script's repo)")
    p.add_argument("--payload", type=pathlib.Path,
                   help="Notes executable to package (default: "
                        "WORKTREE/target/release/notes)")
    p.add_argument("--out", type=pathlib.Path,
                   default=here / "target" / "install-on-open",
                   help="directory for screenshots and logs")
    p.add_argument("--row-timeout", type=float, default=45.0,
                   help="seconds to wait for the palette row (default: 45)")
    p.add_argument("--tab-timeout", type=float, default=120.0,
                   help="seconds to wait for the tab to appear (default: 120)")
    p.add_argument("--frame-timeout", type=float, default=120.0,
                   help="seconds to wait for the app's first frame (default: 120)")
    p.add_argument("--startup-timeout", type=float, default=60.0,
                   help="seconds to wait for the control socket (default: 60)")
    p.add_argument("--backend", default="headless",
                   choices=("headless", "winit", "drm"),
                   help="compositor backend. headless checks the flow with no "
                        "display; winit opens a real window to film "
                        "(default: headless)")
    p.add_argument("--stage-only", action="store_true",
                   help="set the scene and stop: registry up, Notes absent, "
                        "compositor running with the right environment. Nothing "
                        "is driven — the operator types. This is the filming "
                        "mode; Ctrl-C tears it down")
    p.add_argument("--chord", default="super-space",
                   choices=("super-space", "alt-space", "ctrl-k"),
                   help="palette chord to open with. The two global ones "
                        "offer apps this machine does not have; ctrl-k is "
                        "scoped to the focused app and is here to be measured "
                        "against, not to film (default: super-space)")
    p.add_argument("--hold", type=float, default=0.0,
                   help="extra seconds to leave the session up after the "
                        "final capture (for filming a live window)")
    p.add_argument("--record", "--video", dest="record", type=pathlib.Path,
                   help="record the verified interaction to an H.264 MP4 by "
                        "sampling the live compositor (requires ffmpeg)")
    p.add_argument("--size", type=parse_size, default=(1600, 900), metavar="WxH",
                   help="headless capture viewport (default: 1600x900)")
    p.add_argument("--typing-delay", type=float, default=0.22,
                   help="on-screen seconds per typed character (default: 0.22)")
    p.add_argument("--pre-roll", type=float, default=0.65,
                   help="seconds of the desktop before opening search (default: 0.65)")
    p.add_argument("--final-hold", type=float, default=2.0,
                   help="seconds to hold on Notes visibly running (default: 2)")
    p.add_argument("--keep-recording-frames", action="store_true",
                   help="retain the event PNG sequence used for --record")
    args = p.parse_args()

    if min(args.typing_delay, args.pre_roll, args.final_hold) < 0:
        p.error("--typing-delay, --pre-roll and --final-hold cannot be negative")
    if args.record is not None and args.backend != "headless":
        p.error("--record uses --backend headless so --size and output geometry are deterministic")
    if args.record is not None and args.stage_only:
        p.error("--record and --stage-only are different modes and cannot be combined")
    if args.record is not None and min(args.typing_delay, args.pre_roll, args.final_hold) <= 0:
        p.error("recorded beat durations must be positive")

    target = args.worktree / "target" / "debug"
    release_notes = args.payload or args.worktree / "target" / "release" / "notes"
    binaries = {
        "shell": target / "shell",
        # The package on camera should be the binary a user would fetch, not a
        # 300+ MB debug executable.  Keep the compositor debug-built because
        # TILO_TEST_CONTROL is deliberately absent from shipped release builds.
        "notes": release_notes,
        "tilo-install": target / "tilo-install",
        "tilo-launcher": target / "tilo-launcher",
        "app": target / "app",
        "serve-test-registry": target / "examples" / "serve-test-registry",
    }
    missing = [f"{k} ({v})" for k, v in binaries.items() if not v.exists()]
    if missing:
        print("FAIL: not built:\n  " + "\n  ".join(missing), file=sys.stderr)
        print("\n  cargo build -p shell -p notes -p install -p launcher"
              "\n  cargo build --release -p notes"
              "\n  cargo build -p app-cli --bins --examples", file=sys.stderr)
        return 1

    args.out.mkdir(parents=True, exist_ok=True)
    scratch = args.out / "scratch"
    if scratch.exists():
        shutil.rmtree(scratch)
    apps_dir = scratch / "tilo-apps"
    store_config = scratch / "tilo-store"
    store_cache = scratch / "tilo-store-cache"
    for d in (apps_dir, store_config, store_cache):
        d.mkdir(parents=True, exist_ok=True)

    # A HOME of our own, so the palette's file results are a small fixture
    # corpus rather than the operator's actual home directory.
    home_env = stage_home(args.out / "home")
    home = pathlib.Path(home_env["HOME"])

    # One workspace, one strip, nothing in it. The compositor's default
    # bootstrap seeds demo tabs, and a capture of those plus Notes is
    # half somebody else's app.
    tilo_dir = pathlib.Path(home_env["XDG_CONFIG_HOME"]) / "tilo"
    tilo_dir.mkdir(parents=True, exist_ok=True)
    (tilo_dir / "state.json").write_text(json.dumps({
        "workspaces": [{
            "id": 1,
            "room": "00000000-0000-4000-8000-000000000001",
            "name": "Apps",
            "strips": [{"id": 2, "name": "Main", "window_groups": []}],
            "collaborators": [],
            "owner": None,
            "capabilities": [],
            "shared_surfaces": {},
        }],
        "active": 1,
        "active_strip": {"1": 2},
        "active_group": {},
        "collaborators": [],
        "room_authority": {},
    }))

    env_extra = {
        **home_env,
        # The install root. Empty, which is what makes "this machine
        # does not have Notes" true rather than asserted.
        "TILO_APPS_DATA_DIR": str(apps_dir),
        "TILO_STORE_CONFIG_DIR": str(store_config),
        "TILO_STORE_CACHE_DIR": str(store_cache),
        "TILO_APP_BIN": str(binaries["app"]),
        "TILO_INSTALL_BIN": str(binaries["tilo-install"]),
        "TILO_LAUNCHER_BIN": str(binaries["tilo-launcher"]),
        "TILO_HEADLESS_SIZE": f"{args.size[0]}x{args.size[1]}",
    }

    shell_log = args.out / "compositor.log"
    registry_log = args.out / "registry.log"
    palette_png = args.out / "palette.png"
    pending_png = args.out / "pending.png"
    tab_png = args.out / "tab.png"

    print(f"chord: {args.chord}")
    print(f"staged HOME: {home}")
    print(f"install root: {apps_dir}  (empty)")

    with Registry(binaries["serve-test-registry"], binaries["notes"],
                  store_config, registry_log) as reg:
        print(f"registry: {reg.base_url}  serving {PACKAGE_ID} packed from "
              f"{binaries['notes']}")

        # --- 1. offered, and not here ---------------------------------
        probe_env = {**os.environ, **env_extra}
        listing = json.loads(subprocess.run(
            [str(binaries["app"]), "list", "--json"],
            env=probe_env, capture_output=True, text=True, check=True).stdout)
        try:
            check(not listing.get("unreachable"),
                  f"the local registry must be reachable: {listing.get('unreachable')}")
            offered = [a for a in listing["apps"] if a["package_id"] == PACKAGE_ID]
            check(bool(offered),
                  f"{PACKAGE_ID} must be offered: {listing['apps']}")
            check(not (apps_dir / "registry.toml").exists(),
                  f"{apps_dir} must start with nothing installed")
        except Failure as e:
            print(f"FAIL (step 1): {e}", file=sys.stderr)
            return 1
        print(f"  offered: {offered[0]['display_name']} {offered[0]['version']} "
              f"from {offered[0]['registry']!r} (trust: {offered[0]['trust']})")
        print("  installed: nothing")

        # --- 2..4 through the live compositor -------------------------
        with Compositor(binaries, env_extra, shell_log, args.startup_timeout,
                        args.backend) as comp:
            if args.stage_only:
                print("\nthe scene is set — nothing has been driven.\n")
                print(f"  Notes is offered and not installed")
                print(f"  compositor:  {args.backend} backend, "
                      f"log {shell_log}")
                print(f"  socket:      {comp.sock}")
                print(f"\nOpen the palette with Super+Space (or Alt+Space if a host "
                      f"desktop\nis eating Super), type \"{QUERY}\", press Enter.\n")
                print("Ctrl-C to tear it all down. Re-running resets the install "
                      "root,\nso the app is absent again on the next take.")
                try:
                    while comp.proc.poll() is None:
                        time.sleep(1.0)
                except KeyboardInterrupt:
                    print("\ntearing down.")
                return 0
            recorder = None
            try:
                if args.record is not None:
                    # Warm the shell's own catalogue cache before the take.  The
                    # first palette query schedules an asynchronous registry
                    # refresh; filming that implementation detail leaves a
                    # blank result list for a couple of seconds.  This dry run
                    # fetches no package and launches nothing.  The recorded
                    # run below still begins with an empty install root and
                    # performs the complete signed fetch/install/open path.
                    comp.open_palette(args.chord)
                    time.sleep(0.4)
                    comp.type_text(QUERY)
                    time.sleep(0.5)
                    _, warm_row = wait_for_row(comp, args.row_timeout)
                    check_row_wording(warm_row)
                    comp.key("Escape")
                    time.sleep(0.35)

                    recorder = FrameRecorder(comp, args.out / "recording-frames")
                    recorder.start()
                    recorder.capture("desktop", args.pre_roll)

                comp.open_palette(args.chord)
                time.sleep(0.6)
                if recorder is not None:
                    recorder.capture("palette-open", 0.35)
                pal = comp.palette()
                check(pal.get("visible") is True,
                      f"{args.chord} must open the palette; palette was {pal}")
                # Which question the chord asked. `All` is the scope
                # that offers apps this machine does not have; report it
                # rather than assume it, because `ActiveApp` widens to
                # `All` when nothing is focused and the two are then
                # indistinguishable from the outside.
                scope = comp.state().get("chrome", {}).get("search_scope")
                print(f"  scope: {scope}")
                # A check-only run has a cold in-process cache; a filmed run
                # warmed it above so the letters and matching row can arrive
                # at a human pace with no artificial blank pause.
                if args.record is None:
                    time.sleep(2.0)

                # Send the same text events a keyboard does, one readable
                # beat at a time when a camera is running.
                if args.record is not None:
                    for character in QUERY:
                        comp.type_text(character)
                        # Give the real provider/layout pass a tick, then paint
                        # exactly what it produced for this prefix.
                        time.sleep(0.08)
                        recorder.capture(f"query-{comp.palette().get('query', '')}",
                                         args.typing_delay)
                else:
                    comp.type_text(QUERY)
                time.sleep(1.0)
                pal, row = wait_for_row(comp, args.row_timeout)
                check_row_wording(row)
                # Enter takes the selected row, so "the row exists" is
                # not the same claim as "Enter opens Notes".
                check(pal.get("selected_id") == row["id"],
                      f"Enter must be about to activate {row['label']!r} "
                      f"({row['id']}), but the selected row is "
                      f"{pal.get('selected_id')!r} out of {pal.get('rows')}")
                if recorder is not None:
                    recorder.capture("verified-result", 0.45, mirror=palette_png)
                else:
                    comp.snapshot(palette_png)
                print(f"  palette row: {row['label']!r} / "
                      f"{row.get('description')!r}  → {palette_png}")

                # --- 3. Enter ---------------------------------------
                # A keystroke, not a click on the row. The socket takes
                # winit's key names, so this is "Enter"; "Return" is an
                # xkb name and comes back as a parse_error rather than
                # quietly doing nothing.
                before = len(comp.notifications())
                comp.key("Enter")
                time.sleep(0.2)
                after = comp.notifications()
                check(len(after) > before or any(
                    DISPLAY_NAME in n.get("title", "") for n in after),
                    "Enter did not activate the row — nothing happened "
                    f"(notifications: {after})")
                pending = [n for n in after if DISPLAY_NAME in n.get("title", "")]
                if pending:
                    print(f"  activated: {pending[-1]['title']!r} — "
                          f"{pending[-1]['body']!r}")
                # The one place the machine admits the app is coming
                # from somewhere. Captured while it is still up: it is
                # retired by the app starting, not by a timer, so it is
                # gone from the final frame.
                if recorder is not None:
                    recorder.capture("opening", 0.65, mirror=pending_png)
                else:
                    comp.snapshot(pending_png)

                # --- 4. tab, then a frame of its own ----------------
                adopted = wait_for_tab(comp, args.tab_timeout)
                check(bool(adopted),
                      "no tab appeared for Notes. Compositor log: "
                      + " | ".join(app_log_lines(shell_log, PACKAGE_ID)[-6:]))
                print(f"  adopted as a tab: {', '.join(adopted)}")

                frame = wait_for_first_frame(comp, args.frame_timeout)
                comp.key("Escape")  # dismiss the palette before the capture
                time.sleep(0.5)
                if recorder is not None:
                    final_beat = args.final_hold / 3.0
                    recorder.capture("notes-final-1", final_beat, mirror=tab_png)
                else:
                    comp.snapshot(tab_png)
                check(frame is not None,
                      "the compositor tracked no frames for the tab at all")
                check((frame.get("frame_seq") or 0) > 0,
                      "the tab never presented a frame of its own — "
                      f"{tab_png} is the first-frame placeholder, not Notes "
                      f"({frame})")
                check_frame_in_viewport(frame, args.size)
                check(png_size(tab_png) == args.size,
                      f"poster is {png_size(tab_png)[0]}x{png_size(tab_png)[1]}, "
                      f"expected {args.size[0]}x{args.size[1]}")
                print(f"  first frame: frame_seq={frame['frame_seq']}  → {tab_png}")

                if recorder is not None:
                    # Three independently painted final frames: the timeline
                    # ends on the actual client, and one lucky snapshot cannot
                    # stand in for a stable presented state.
                    recorder.capture("notes-final-2", final_beat)
                    recorder.capture("notes-final-3", final_beat)
                    recorder.stop()
                    final_notes_frames = [
                        current for current in comp.foreign_frames()
                        if PACKAGE_ID in str(current.get("app_id", ""))
                        and (current.get("frame_seq") or 0) > 0
                    ]
                    check(bool(final_notes_frames),
                          "the final recorded state does not contain a presented "
                          f"frame from {PACKAGE_ID}")
                    check_frame_in_viewport(final_notes_frames[0], args.size)
                    print("  final captured client: "
                          f"{final_notes_frames[0].get('app_id')} "
                          f"frame_seq={final_notes_frames[0].get('frame_seq')} "
                          f"rect={final_notes_frames[0].get('rect')}")
                    recorder.encode(args.record)
                    check(video_size(args.record) == args.size,
                          f"video is {video_size(args.record)[0]}x"
                          f"{video_size(args.record)[1]}, expected "
                          f"{args.size[0]}x{args.size[1]}")
                    print(f"  recorded: {len(recorder.samples)} verified event frames "
                          f"→ {args.record}")
                    if not args.keep_recording_frames:
                        shutil.rmtree(recorder.frames_dir)

                # With a tab in front of you the two chords stop being
                # interchangeable, and this is where that becomes
                # visible. On the empty workspace this run starts from,
                # `Ctrl+K` reports `All` as well — `ActiveApp` widens
                # when nothing is focused — so a check run only at the
                # start cannot tell them apart. Measured after the app
                # is up and focused, it can.
                for chord in ("super-space", "ctrl-k"):
                    comp.open_palette(chord)
                    time.sleep(0.5)
                    print(f"  with Notes focused, {chord} → scope "
                          f"{comp.state().get('chrome', {}).get('search_scope')}")
                    comp.key("Escape")
                    time.sleep(0.3)

                if args.hold > 0:
                    print(f"  holding for {args.hold:.0f}s ...")
                    time.sleep(args.hold)
            except (Failure, RuntimeError, OSError, subprocess.CalledProcessError) as e:
                if recorder is not None:
                    try:
                        recorder.stop()
                    except RuntimeError as record_error:
                        print(f"      recorder teardown: {record_error}", file=sys.stderr)
                print(f"FAIL: {e}", file=sys.stderr)
                print(f"      compositor log: {shell_log}", file=sys.stderr)
                print(f"      registry log:   {registry_log}", file=sys.stderr)
                return 1

    print("\nOK — an app this machine did not have opened as a tab and drew "
          "itself, from one keystroke in the search box.")
    print(f"  {palette_png}")
    print(f"  {pending_png}")
    print(f"  {tab_png}")
    if args.record is not None:
        print(f"  {args.record}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
