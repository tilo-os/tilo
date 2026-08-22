#!/usr/bin/env python3
"""Three qOS sessions in one room, and an agent that drives one of them.

Starts three real compositors — separate state directories, separate
libp2p keypairs, separate user identities, separate control sockets:

  quinten  hosts the room and owns it
  xaver    joins it by invite
  agent    joins it too, and from then on is driven *only* by the action
           API: semantic invocation by `(provider, id)`, never by a
           click, a keystroke, or a pixel

Three beats, about a minute:

  1. one room, three sessions   — proven by reading quinten's room back
                                  out of xaver's and the agent's own state
  2. the agent sees the surface — the shell exports a typed, addressable
                                  action catalogue, and enumerating it
                                  provably changes nothing
  3. the agent acts             — it invokes an action by address and the
                                  effect lands, with a counter proving no
                                  synthetic input was used

Every claim is read back from a session that did **not** make the change.
Where that is impossible with today's compositor the transcript prints a
GAP and moves on; it never prints a success it did not verify.

    scripts/yc-demo.py
    scripts/yc-demo.py --keep
    scripts/yc-demo.py --record            # …and write a 1920x1080 video

Runs against target/debug/shell. There is no mock and no fixture path:
if the binary is missing the script exits rather than simulating it.
"""
from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import signal
import socket
import subprocess
import sys
import threading
import time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# The action the agent invokes, as a stable `(provider, id)` address from
# the shell's own action provider — the same registry that feeds the
# command palette, reached with no palette and no keystroke.
AGENT_PROVIDER = "actions"
AGENT_ACTION_ID = "room.activity.open"
# Its effect, as a key under `chrome.overlays` in `state_dump`.
AGENT_ACTION_EFFECT = "room_activity"

# How long to wait for a change to cross between sessions.
SYNC_TIMEOUT_S = 25.0


# --------------------------------------------------------------------
# control socket
# --------------------------------------------------------------------


def call(sock_path: str, payload: dict, timeout: float = 30.0) -> dict:
    """Send one command, read one reply."""
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
        return json.loads(buf.split(b"\n", 1)[0])
    finally:
        s.close()


def ok(resp: dict) -> dict:
    if "err" in resp:
        raise RuntimeError(f"compositor refused: {resp['err']}")
    return resp


class Session:
    """One running compositor, addressed by its control socket."""

    def __init__(self, name: str, user: str, run_dir: str, sock: str, binary: str,
                 size: str, runtime_dir: str):
        self.name = name
        self.user = user
        self.dir = run_dir
        self.sock = sock
        self.binary = binary
        self.size = size
        # Each session needs its own XDG_RUNTIME_DIR: the compositor
        # binds its collab broker at <runtime>/qos/services/collab.sock,
        # and two sessions sharing one runtime dir are two sessions
        # contending for one socket. Keep it short — sun_path is ~108 bytes.
        self.runtime_dir = runtime_dir
        self.proc: subprocess.Popen | None = None
        self.log_path = os.path.join(run_dir, "compositor.log")
        self._log = None
        # Every synthetic-input command this session has been sent. The
        # agent's whole claim is that it never uses one, and a counter is
        # a mechanical proof of that where a promise is not.
        self.input_ops = 0

    @property
    def collab_sock(self) -> str:
        return os.path.join(self.runtime_dir, "qos", "services", "collab.sock")

    # -- lifecycle ---------------------------------------------------

    def start(self) -> None:
        os.makedirs(os.path.join(self.dir, "config"), exist_ok=True)
        os.makedirs(os.path.join(self.dir, "keys"), exist_ok=True)
        os.makedirs(self.runtime_dir, mode=0o700, exist_ok=True)
        env = dict(os.environ)
        # No display, and nothing inherited that would make the three
        # sessions share state.
        env.pop("DISPLAY", None)
        env.pop("WAYLAND_DISPLAY", None)
        env.pop("QUICK_COLLAB_SOCK", None)
        env.pop("QUICK_COLLAB_CAPNP_SOCK", None)
        env.update(
            XDG_RUNTIME_DIR=self.runtime_dir,
            XDG_CONFIG_HOME=os.path.join(self.dir, "config"),
            XDG_DATA_HOME=os.path.join(self.dir, "data"),
            XDG_CACHE_HOME=os.path.join(self.dir, "cache"),
            XDG_STATE_HOME=os.path.join(self.dir, "state"),
            # Named outright rather than left to `dirs::config_dir()`.
            # That helper ignores a *relative* XDG_CONFIG_HOME, so a run
            # started with a relative --out gets no isolation from it at
            # all and persists into the real ~/.config/qos/state.json —
            # three demo sessions writing workspaces, owned by
            # identities nobody at this machine can administer, into
            # somebody's live desktop. `--out` is absolutised for the
            # same reason; this is the belt to that pair of braces.
            QOS_STATE_PATH=os.path.join(self.dir, "config", "state.json"),
            QOS_USER_ID=self.user,
            QOS_PEER_KEY_PATH=os.path.join(self.dir, "keys", "peer-key"),
            QOS_FS_ACL_PATH=os.path.join(self.dir, "config", "fs-capabilities.json"),
            QOS_TEST_CONTROL=self.sock,
            NIRIOS_HEADLESS_SIZE=self.size,
            RUST_LOG=env.get("RUST_LOG", "info"),
        )
        self._log = open(self.log_path, "w")
        self.proc = subprocess.Popen(
            [self.binary, "--backend", "headless", "--enable-peer-sync"],
            cwd=REPO, env=env, stdout=self._log, stderr=subprocess.STDOUT,
            # Its own session, so `--keep` actually keeps: otherwise the
            # children share this process's group and die with it the
            # moment the script returns, which made "left running" a
            # message printed about three processes that were already
            # gone. `stop()` still reaches them by pid.
            start_new_session=True,
        )

    def wait_ready(self, timeout: float = 90.0) -> None:
        deadline = time.time() + timeout
        while time.time() < deadline:
            if self.proc and self.proc.poll() is not None:
                raise RuntimeError(
                    f"{self.name} exited before binding its control socket; "
                    f"see {self.log_path}")
            if os.path.exists(self.sock):
                # Bound is not the same as answering.
                try:
                    self.state()
                    return
                except (OSError, RuntimeError):
                    pass
            time.sleep(0.2)
        raise RuntimeError(f"{self.name} never answered on {self.sock}")

    def stop(self) -> None:
        if self.proc and self.proc.poll() is None:
            self.proc.send_signal(signal.SIGTERM)
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.proc.kill()
        if self._log:
            self._log.close()
            self._log = None

    # -- reading (never counts as input) -----------------------------

    def state(self) -> dict:
        return ok(call(self.sock, {"op": "state_dump"}))["state"]

    def chrome(self) -> dict:
        return self.state()["chrome"]

    def tree(self) -> dict:
        return self.state()["workspace_tree"]

    def active_workspace(self) -> dict:
        tree = self.tree()
        active = tree["active"]
        for ws in tree["workspaces"]:
            if ws["id"] == active:
                return ws
        raise RuntimeError(f"{self.name}: no active workspace in the tree")

    def overlays(self) -> dict:
        return self.chrome()["overlays"]

    def palette(self) -> dict:
        return self.chrome()["palette"]

    # -- the action API (semantic; no input, no pixels) --------------

    def enumerate_actions(self, query: str = "") -> dict:
        """The exported action surface. Read-only by construction."""
        return ok(call(self.sock, {"op": "actions_enumerate", "query": query}))["answer"]

    def invoke_action(self, provider: str, action_id: str) -> dict:
        """Invoke one action by its stable address. Raw response, so a
        refusal is a result rather than an exception."""
        return call(self.sock, {"op": "actions_invoke",
                                "provider": provider, "id": action_id})

    # -- synthetic input (counted) -----------------------------------

    def key(self, name: str) -> None:
        self.input_ops += 1
        ok(call(self.sock, {"op": "key", "key": name}))

    def type_text(self, text: str) -> None:
        self.input_ops += 1
        # One command per few hundred characters keeps each request under
        # the socket's line cap while still arriving as ordinary keys.
        for i in range(0, len(text), 200):
            ok(call(self.sock, {"op": "type", "text": text[i:i + 200]}))

    def modifiers(self, ctrl=False, shift=False, alt=False, logo=False) -> None:
        self.input_ops += 1
        ok(call(self.sock, {"op": "modifiers", "ctrl": ctrl, "shift": shift,
                            "alt": alt, "logo": logo}))

    # -- painting ----------------------------------------------------

    def snapshot(self, path: str) -> str:
        os.makedirs(os.path.dirname(path), exist_ok=True)
        ok(call(self.sock, {"op": "snapshot", "path": path}))
        return path

    def settle(self, seconds: float = 0.5) -> None:
        """Let the compositor paint."""
        time.sleep(seconds)

    # -- palette (people only) ---------------------------------------

    def run_palette_command(self, query: str, expect_id: str | None = None) -> None:
        """Open the palette, type `query`, activate the top row."""
        self.modifiers(logo=True)
        self.key("k")
        self.modifiers()
        self.settle(0.35)
        self.type_text(query)
        self.settle(0.6)
        selected = self.palette()["selected_id"]
        if expect_id is not None and selected != expect_id:
            rows = [r["id"] for r in self.palette()["rows"]][:6]
            raise RuntimeError(
                f"{self.name}: palette query {query!r} selected {selected!r}, "
                f"expected {expect_id!r} (rows: {rows})")
        self.key("Enter")
        self.settle()


# --------------------------------------------------------------------
# recording
# --------------------------------------------------------------------
#
# There is no screen recorder on this machine that can see a Wayland
# surface, and none is wanted: the compositor renders its own frames on
# demand over the control socket, so a recording here is a real sampled
# render of each session rather than a grab of three windows somebody
# had to arrange first. Nothing is drawn that a session did not draw.
#
# The sample rate is whatever `snapshot` sustains, which is not much:
# it re-paints the scene into a fresh offscreen surface and PNG-encodes
# it *on the compositor's own event loop*, so the session is blocked for
# every millisecond of it. Measured on this machine, one session alone
# and then all three at once:
#
#     1280x800   ~900 ms   1.1 /s solo   0.86 /s ×3
#      640x880   ~630 ms   1.6 /s solo   1.23 /s ×3
#      640x400   ~450 ms   2.2 /s solo
#
# So this is a ~1 Hz recording however it is cut. It is written out at
# each frame's true timestamp — the video runs in real time and is
# simply sparse. Frames are never interpolated or invented to make it
# look faster than it was; the only frame ever repeated is the last one
# of a session whose track ended early, held to keep the panes in step.
# Where the video needs to be longer, the demo is given more real time
# to be longer in (`--record-dwell`), never more frames than it took.

# The composed frame. Three panes side by side is the whole claim —
# three sessions, all three visible at once. 1920 across three columns
# is 640 each, so a session recorded at 640 wide tiles in at 1:1 with
# nothing resampled, which is the difference between a readable pane
# and a blurred one. The bands above and below it are sized from
# whatever vertical space is left over, and carry the session's name,
# the current beat, the current step and the claim being checked — so
# the video explains itself with no voiceover and nothing on it is
# decoration.
VIDEO_W, VIDEO_H = 1920, 1080
COLUMN_W = VIDEO_W // 3
# The size each session is recorded at when `--record` picks for itself:
# exactly one column wide, and short enough to leave the caption bands
# room, so three of them tile into 1920x1080 untouched.
RECORD_SIZE = f"{COLUMN_W}x880"
# Vertical space the bands need between them. A pane taller than this
# leaves gets scaled down to make room — and says so.
BANDS_H = 200
TOP_BAND_MAX = 68

BG = "0x0B0D12"       # the shell's own backdrop, so each pane sits in it
EDGE = "0x2A3346"
FG = "0xE9ECF2"
DIM = "0x93A1BC"
PASS_C = "0x7BD88F"
FAIL_C = "0xFF6B6B"
GAP_C = "0xF0B849"

# What the transcript's own marks look like on screen, which band they
# sit in, and which of them supersede which: a new beat clears the step
# under it, a new step clears the claim under that. Lower rank wins.
MARK_STYLE = {
    "beat": (0, "beat", 22, FG, True),
    "step": (1, "step", 21, DIM, False),
    "pass": (2, "claim", 19, PASS_C, False),
    "fail": (2, "claim", 19, FAIL_C, False),
    "gap": (2, "claim", 19, GAP_C, False),
}

# What each session is, in the video. Roles, not decoration: the whole
# point of the third pane is that it is driven differently.
ROLES = {
    "quinten": "hosts the room",
    "xaver": "joined by invite",
    "agent": "driven only by (provider, id)",
}

FONTS_REGULAR = (
    "/usr/share/fonts/liberation-sans-fonts/LiberationSans-Regular.ttf",
    "/usr/share/fonts/adwaita-sans-fonts/AdwaitaSans-Regular.ttf",
    "/usr/share/fonts/google-droid-sans-fonts/DroidSans.ttf",
)
FONTS_BOLD = (
    "/usr/share/fonts/liberation-sans-fonts/LiberationSans-Bold.ttf",
) + FONTS_REGULAR

# A snapshot blocks the session it is taken from, so the capture loop
# always yields this long between frames even when asked for "as fast as
# possible". Without it the demo is competing with its own camera for
# every reply, and the waits it depends on start timing out.
MIN_CAPTURE_GAP_S = 0.15


def _pick_font(candidates: tuple[str, ...]) -> str:
    for path in candidates:
        # A colon would be read as an option separator inside the filter
        # graph, so a path carrying one is no use to us even if it exists.
        if os.path.exists(path) and ":" not in path:
            return path
    raise RuntimeError(
        "no usable font for the video labels; looked for:\n  "
        + "\n  ".join(candidates))


def _even(n: float) -> int:
    """Sizes h264 can encode without the scaler quietly rounding for us."""
    return int(n) - (int(n) % 2)


def _one_line(text: str, limit: int = 108) -> str:
    flat = " ".join(text.split())
    return flat if len(flat) <= limit else flat[: limit - 1] + "…"


class Recorder:
    """Timestamped frames pulled from every session while the demo runs.

    Capture threads are readers: `snapshot` re-paints and writes a PNG,
    and never enters the input path, so the agent's "no synthetic input"
    claim is as true during a recording as without one — `input_ops` is
    untouched by anything in here.
    """

    def __init__(self, sessions: list[Session], directory: str,
                 interval: float, dwell: float = 0.0) -> None:
        self.sessions = sessions
        self.dir = directory
        self.interval = interval
        self.dwell_s = dwell
        self.frames: dict[str, list[tuple[float, str]]] = {
            s.name: [] for s in sessions}
        self.marks: list[tuple[float, str, str]] = []
        self.errors: list[str] = []
        self._stop = threading.Event()
        self._threads: list[threading.Thread] = []
        self.started_at = 0.0
        self.stopped_at = 0.0

    # -- capture -----------------------------------------------------

    def start(self) -> None:
        os.makedirs(self.dir, exist_ok=True)
        self.started_at = time.time()
        for s in self.sessions:
            t = threading.Thread(target=self._capture, args=(s,),
                                 name=f"capture-{s.name}", daemon=True)
            t.start()
            self._threads.append(t)

    def _capture(self, s: Session) -> None:
        directory = os.path.join(self.dir, "frames", s.name)
        os.makedirs(directory, exist_ok=True)
        index = 0
        while not self._stop.is_set():
            began = time.time()
            rel = f"frames/{s.name}/{index:06d}.png"
            try:
                s.snapshot(os.path.join(self.dir, rel))
            except Exception as exc:  # noqa: BLE001
                # One session going quiet ends its own track and nothing
                # else. The gap is reported and padded visibly rather
                # than closed over.
                self.errors.append(
                    f"{s.name}: capture stopped after {index} frame(s): "
                    f"{type(exc).__name__}: {exc}")
                return
            self.frames[s.name].append((began, rel))
            index += 1
            self._stop.wait(
                max(MIN_CAPTURE_GAP_S, self.interval - (time.time() - began)))

    def mark(self, kind: str, text: str) -> None:
        """Timestamp a line of the transcript. Called from the demo
        thread only, so the list stays in the order it reads in."""
        self.marks.append((time.time(), kind, _one_line(text)))

    def dwell(self) -> None:
        """Hold on the moment just marked.

        The one honest way to make the video longer. Every second spent
        here is a second the demo really took, sampled like any other —
        as against holding a frame on screen for longer than the session
        held it, which would be inventing time the run did not have.
        """
        if self.dwell_s > 0:
            time.sleep(self.dwell_s)

    def stop(self) -> None:
        self._stop.set()
        for t in self._threads:
            t.join(timeout=30)
        self._threads.clear()
        self.stopped_at = time.time()

    # -- what actually happened --------------------------------------

    def rate(self) -> float:
        """Frames per second per session, as achieved."""
        span = max(1e-6, self.stopped_at - self.started_at)
        counts = [len(f) for f in self.frames.values()]
        return (sum(counts) / len(counts)) / span if counts else 0.0

    def report(self) -> None:
        span = self.stopped_at - self.started_at
        print(f"\ncaptured over {span:.1f}s of demo:")
        for s in self.sessions:
            n = len(self.frames[s.name])
            print(f"  {s.name:8} {n:4d} frames  "
                  f"{n / max(1e-6, span):.2f} frames/s")
        for e in self.errors:
            print(f"  ! {e}")

    # -- composition -------------------------------------------------

    def _window(self) -> tuple[float, float]:
        firsts = [f[0][0] for f in self.frames.values() if f]
        lasts = [f[-1][0] for f in self.frames.values() if f]
        if not firsts:
            raise RuntimeError("no frames were captured from any session")
        return min(firsts), max(lasts)

    def _concat_list(self, name: str, t_start: float, t_end: float,
                     fps: int) -> tuple[str, float]:
        """One session's track, on the shared timeline.

        Each frame is shown from when it was taken until the next one
        was, so the video's clock is the demo's clock. A session whose
        track starts late or ends early holds its nearest frame to cover
        the difference — that is the only repetition in the file, and it
        is reported rather than hidden.

        The three tracks are made to come out exactly the same length,
        which is what lets the stack end cleanly instead of trailing a
        dead tail while it waits for the slowest input to finish.
        """
        frames = self.frames[name]
        if not frames:
            raise RuntimeError(f"{name}: no frames to compose")
        tick = 1.0 / fps
        # ffmpeg's concat demuxer drops exactly one frame off the tail,
        # so the track carries one tick more than the window it covers.
        target = (t_end - t_start) + tick
        spans = []
        for i, (t, _) in enumerate(frames):
            begins = t_start if i == 0 else t
            ends = frames[i + 1][0] if i + 1 < len(frames) else t_end + tick
            spans.append(max(tick, ends - begins))
        # Clamping short gaps up to one tick can only have added time;
        # take it back off the held tail so the total is exact.
        drift = sum(spans) - target
        spans[-1] = max(tick, spans[-1] - drift)
        lines = ["ffconcat version 1.0"]
        for (_, rel), seconds in zip(frames, spans):
            lines.append(f"file '{rel}'")
            lines.append(f"duration {seconds:.4f}")
        with open(os.path.join(self.dir, f"{name}.txt"), "w") as f:
            f.write("\n".join(lines) + "\n")
        return f"{name}.txt", sum(spans)

    def _caption(self, slug: str, text: str) -> str:
        """Caption text goes in a file, not in the filter graph: drawtext
        reads it verbatim, so an em-dash or an apostrophe in the
        transcript cannot break the graph it is quoted into."""
        directory = os.path.join(self.dir, "captions")
        os.makedirs(directory, exist_ok=True)
        rel = f"captions/{slug}.txt"
        with open(os.path.join(self.dir, rel), "w") as f:
            f.write(text)
        return rel

    def _spans(self, t_start: float, t_end: float, fps: int) -> list[tuple]:
        """Turn the transcript marks into (kind, start, end, text) spans
        on the video clock.

        A mark stays up until something at least as important replaces
        it: a new beat clears the step and the claim beneath it, a new
        step clears the claim. So the frame always reads top to bottom
        as beat, then what is being done, then what was just proven.

        Marks are left where they happened. Claims in particular arrive
        in bursts, and the ones superseded inside a single frame are
        dropped rather than nudged along the timeline to make room —
        a caption sliding off the moment it describes would be the one
        thing on this video that is not when it says it is.
        """
        out = []
        for i, (t, kind, text) in enumerate(self.marks):
            rank = MARK_STYLE[kind][0]
            nxt = next((m[0] for m in self.marks[i + 1:]
                        if MARK_STYLE[m[1]][0] <= rank), t_end)
            begins = max(0.0, t - t_start)
            ends = max(begins, min(t_end, nxt) - t_start)
            if ends - begins >= 1.0 / fps:
                out.append((kind, begins, ends, text))
        return out

    def compose(self, out_path: str, fps: int, size: str) -> dict:
        if shutil.which("ffmpeg") is None or shutil.which("ffprobe") is None:
            raise RuntimeError("ffmpeg and ffprobe are needed to compose the "
                               "video; the frames are still on disk")
        regular, bold = _pick_font(FONTS_REGULAR), _pick_font(FONTS_BOLD)
        t_start, t_end = self._window()
        # Nothing to time a video against if the demo never got going.
        if t_end - t_start < 1.0:
            raise RuntimeError(
                f"only {t_end - t_start:.2f}s of capture; too little to compose")

        source_w, source_h = (int(v) for v in size.lower().split("x"))
        pane_w, pane_h = source_w, source_h
        if pane_w > COLUMN_W or pane_h > VIDEO_H - BANDS_H:
            k = min(COLUMN_W / source_w, (VIDEO_H - BANDS_H) / source_h)
            pane_w, pane_h = _even(source_w * k), _even(source_h * k)
            print(f"  {size} does not tile into {VIDEO_W}x{VIDEO_H}; panes are "
                  f"resampled to {pane_w}x{pane_h}. Record at "
                  f"{RECORD_SIZE} for 1:1.")
        pane_x = (COLUMN_W - pane_w) // 2
        # Whatever vertical space the pane leaves becomes the two bands:
        # a shallow one over it for the title and the beat, the rest
        # under it for the labels, the step, the claim and the footnote.
        spare = VIDEO_H - pane_h
        pane_y = min(TOP_BAND_MAX, spare // 3)
        under = pane_y + pane_h
        title_y, beat_y = 8, 38
        label_y, step_y = under + 8, under + 40
        claim_y, footer_y = under + 70, under + 100
        band_y = {"beat": beat_y, "step": step_y, "claim": claim_y}

        lists, lengths = [], []
        for s in self.sessions:
            rel, seconds = self._concat_list(s.name, t_start, t_end, fps)
            lists.append(rel)
            lengths.append(seconds)
            held = t_end - self.frames[s.name][-1][0]
            if held > 1.5 / fps:
                print(f"  {s.name}: {len(self.frames[s.name])} frames, track "
                      f"ends {held:.1f}s early; its last frame is held to fill "
                      f"the gap")
        # `shortest` below is only safe because of this: the three tracks
        # were padded to one window, so nothing of any of them is cut.
        if max(lengths) - min(lengths) > 1.0 / fps:
            raise RuntimeError(
                f"tracks came out different lengths ({lengths}); refusing to "
                f"stack them, one would be truncated")

        def text(font: str, rel: str, size_px: int, color: str, y: int,
                 enable: str | None = None) -> str:
            bits = [f"fontfile={font}", f"textfile={rel}", f"fontcolor={color}",
                    f"fontsize={size_px}", "x=(w-text_w)/2", f"y={y}"]
            if enable:
                bits.append(f"enable='{enable}'")
            return "drawtext=" + ":".join(bits)

        parts = []
        for i, s in enumerate(self.sessions):
            chain = [f"fps={fps}"]
            # No scale filter at all when the session already renders at
            # the pane's size. Resampling a 1:1 frame would cost the
            # legibility the size was chosen for.
            if (pane_w, pane_h) != (source_w, source_h):
                chain.append(f"scale={pane_w}:{pane_h}:flags=lanczos")
            label = f"{s.name} · {ROLES[s.name]}" if s.name in ROLES else s.name
            chain += [
                "format=rgb24",
                f"pad={COLUMN_W}:{VIDEO_H}:{pane_x}:{pane_y}:color={BG}",
                f"drawbox=x={pane_x}:y={pane_y}:w={pane_w}:h={pane_h}:"
                f"color={EDGE}:t=1",
                text(bold, self._caption(f"name{i}", label), 20, FG, label_y),
            ]
            parts.append(f"[{i}:v]" + ",".join(chain) + f"[c{i}]")
        # `shortest`: every input is the same length by construction, so
        # this ends the stack when they end rather than letting it hold a
        # dead tail while ffmpeg waits out the last input's EOF.
        parts.append("[c0][c1][c2]hstack=inputs=3:shortest=1[row]")

        rate = self.rate()
        row = [
            text(bold, self._caption(
                "title", "qOS · three sessions, one room"), 24, FG, title_y),
        ]
        for i, (kind, a, b, txt) in enumerate(self._spans(t_start, t_end, fps)):
            _, band, size_px, color, is_bold = MARK_STYLE[kind]
            row.append(text(bold if is_bold else regular,
                            self._caption(f"m{i:03d}", txt), size_px, color,
                            band_y[band], f"between(t,{a:.3f},{b:.3f})"))
        # Say on the artifact itself what it is. Someone watching a 1 Hz
        # video should be told it is 1 Hz by the video, not find out.
        scaling = ("at 1:1" if (pane_w, pane_h) == (source_w, source_h)
                   else f"resampled from {source_w}x{source_h}")
        row.append(text(regular, self._caption(
            "footer",
            f"every pane is that session's own renderer {scaling}, sampled "
            f"over its control socket at {rate:.1f} frames/s · real time, "
            f"no interpolation"), 15, DIM, footer_y))
        row.append("format=yuv420p")
        parts.append("[row]" + ",".join(row) + "[v]")

        graph = ";".join(parts)
        with open(os.path.join(self.dir, "filter.txt"), "w") as f:
            f.write(graph + "\n")

        cmd = ["ffmpeg", "-y", "-v", "error"]
        for rel in lists:
            cmd += ["-f", "concat", "-safe", "0", "-i", rel]
        cmd += ["-filter_complex", graph, "-map", "[v]",
                "-c:v", "libx264", "-preset", "medium", "-crf", "20",
                "-pix_fmt", "yuv420p", "-r", str(fps),
                "-movflags", "+faststart", os.path.abspath(out_path)]
        # Relative paths throughout the graph, run from the recording
        # directory: a run directory with a colon or a quote in its name
        # would otherwise be read as filter syntax.
        done = subprocess.run(cmd, cwd=self.dir, capture_output=True, text=True)
        if done.returncode != 0:
            raise RuntimeError(
                f"ffmpeg failed ({done.returncode}):\n{done.stderr.strip()}")
        return probe_video(os.path.abspath(out_path))


def probe_video(path: str) -> dict:
    """Read the finished file back. What ffmpeg was asked for and what it
    wrote are different claims, and only the second one is the artifact."""
    done = subprocess.run(
        ["ffprobe", "-v", "error", "-select_streams", "v:0", "-count_frames",
         "-show_entries",
         "stream=width,height,nb_read_frames,r_frame_rate,duration,codec_name",
         "-of", "json", path],
        capture_output=True, text=True)
    if done.returncode != 0:
        raise RuntimeError(f"ffprobe failed: {done.stderr.strip()}")
    streams = json.loads(done.stdout).get("streams") or []
    if not streams:
        raise RuntimeError(f"{path} has no video stream")
    s = streams[0]
    return {
        "path": path,
        "width": int(s["width"]),
        "height": int(s["height"]),
        "frames": int(s["nb_read_frames"]),
        "fps": s["r_frame_rate"],
        "duration": float(s["duration"]),
        "codec": s["codec_name"],
        "bytes": os.path.getsize(path),
    }


# --------------------------------------------------------------------
# transcript
# --------------------------------------------------------------------

FAILURES: list[str] = []
GAPS: list[str] = []

RULE = "-" * 70

# Set by `--record`. The transcript is the demo's narration, so when a
# recording is running the same two calls that print a beat or a step
# also timestamp it, and the video carries the narration it belongs to
# rather than needing one written separately afterwards.
RECORDER: Recorder | None = None


def banner(title: str) -> None:
    print(f"\n{RULE}\n  {title}\n{RULE}", flush=True)
    if RECORDER:
        RECORDER.mark("beat", title)
        RECORDER.dwell()


def step(text: str) -> None:
    print(f"\n  {text}", flush=True)
    if RECORDER:
        RECORDER.mark("step", text)
        RECORDER.dwell()


def claim(description: str, actual, expected) -> None:
    good = actual == expected
    print(f"    [{'PASS' if good else 'FAIL'}] {description}", flush=True)
    if not good:
        print(f"           expected {expected!r}", flush=True)
        print(f"           observed {actual!r}", flush=True)
        FAILURES.append(description)
    if RECORDER:
        RECORDER.mark("pass" if good else "fail",
                      f"{'PASS' if good else 'FAIL'}   {description}")


def gap(description: str) -> None:
    """Something this run could not verify. Its own mark, so it can never
    be mistaken for a pass."""
    print(f"    [GAP ] {description}", flush=True)
    GAPS.append(description)
    if RECORDER:
        RECORDER.mark("gap", f"GAP   {description}")


def note(text: str) -> None:
    print(f"     .   {text}", flush=True)


def wait_for(fn, timeout: float = SYNC_TIMEOUT_S, interval: float = 0.25):
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        last = fn()
        if last:
            return last
        time.sleep(interval)
    return last


def peer_id_of(s: Session) -> str:
    try:
        with open(s.log_path) as f:
            for line in f:
                # tracing paints field names and separators, so the
                # escapes come off before the split.
                plain = re.sub(r"\x1b\[[0-9;]*m", "", line)
                if "local_peer_id=" in plain:
                    return plain.split("local_peer_id=")[1].split()[0].strip()
    except OSError:
        pass
    return "<unknown>"


def mint_invite(host: Session) -> str | None:
    host.run_palette_command("invite peer", expect_id="result:0")
    token = wait_for(lambda: host.chrome()["peer_sync"]["invite_token"], timeout=20)
    # Dismiss the card: it is a modal over the middle of the screen.
    host.key("Escape")
    host.settle(0.3)
    return token


def join_room(guest: Session, token: str, by_action_api: bool) -> bool:
    """Put `guest` into the room `token` names.

    `by_action_api` opens the connect prompt by invoking its stable
    address rather than by driving the palette. The token itself still
    has to be typed either way — the compositor has no semantic
    "join this room" op, which the caller records as a GAP.
    """
    if by_action_api:
        resp = guest.invoke_action(AGENT_PROVIDER, "peer.connect")
        if "err" in resp:
            raise RuntimeError(f"{guest.name}: peer.connect refused: {resp['err']}")
    else:
        guest.run_palette_command("connect to peer", expect_id="result:0")
    if not wait_for(lambda: guest.chrome()["peer_sync"]["connect_input"] is not None,
                    timeout=15):
        return False
    guest.type_text(token)
    guest.key("Enter")
    joined = wait_for(
        lambda: (guest.chrome()["peer_sync"]["status"] or "").startswith("Joined"),
        timeout=15)
    guest.key("Escape")
    guest.settle(0.3)
    return bool(joined)


# --------------------------------------------------------------------
# the demonstration
# --------------------------------------------------------------------


def beat_one(quinten: Session, xaver: Session, agent: Session, shots: str) -> dict:
    banner("1.  one room, three sessions")

    ids = {s.name: peer_id_of(s) for s in (quinten, xaver, agent)}
    for s in (quinten, xaver, agent):
        note(f"{s.name:8} pid {s.proc.pid:<8} user {s.user:<8} peer {ids[s.name]}")
    claim("three separate processes",
          len({s.proc.pid for s in (quinten, xaver, agent)}), 3)
    claim("three separate libp2p identities", len(set(ids.values())), 3)

    host_ws = quinten.active_workspace()
    note(f"quinten's room {host_ws['room']}  named {host_ws['name']!r}")
    claim("quinten owns his room", host_ws["owner"], quinten.user)

    step("xaver joins by invite")
    token = mint_invite(quinten)
    claim("quinten minted an invite token", bool(token), True)
    if not token:
        raise SystemExit("no invite token: the rest cannot run")
    claim("xaver joined", join_room(xaver, token, by_action_api=False), True)

    step("the agent joins by invite")
    token2 = mint_invite(quinten)
    claim("quinten minted a second token", bool(token2), True)
    if not token2:
        raise SystemExit("no second token: the agent cannot join")
    claim("the agent joined", join_room(agent, token2, by_action_api=True), True)
    # The connect *prompt* was opened by address; the token itself had to
    # be typed, because the control socket has no semantic join op.
    gap("joining has no semantic op — the invite token had to be typed "
        "(only the prompt was opened by address)")

    step("read quinten's room back out of the other two sessions")
    # The verification that matters: neither of these is quinten's own
    # state. Each guest independently reports the room it now holds.
    claim("xaver's own state holds quinten's room",
          xaver.active_workspace()["room"], host_ws["room"])
    claim("the agent's own state holds quinten's room",
          agent.active_workspace()["room"], host_ws["room"])
    claim("xaver's own copy names quinten as owner",
          xaver.active_workspace()["owner"], quinten.user)
    claim("the agent's own copy names quinten as owner",
          agent.active_workspace()["owner"], quinten.user)
    claim("xaver's own copy carries quinten's name for the room",
          xaver.active_workspace()["name"], host_ws["name"])
    claim("the agent's own copy carries it too",
          agent.active_workspace()["name"], host_ws["name"])
    quinten.snapshot(os.path.join(shots, "01-quinten.png"))
    xaver.snapshot(os.path.join(shots, "02-xaver.png"))
    return host_ws


def beat_two(agent: Session, shots: str) -> list[dict]:
    banner("2.  the agent sees a typed, addressable surface")

    step("enumerate everything this session exports")
    surface = agent.enumerate_actions("")
    actions = surface["actions"]
    providers: dict[str, int] = {}
    for a in actions:
        providers[a["provider"]] = providers.get(a["provider"], 0) + 1
    note(f"{surface['count']} addressable actions across "
         f"{len(providers)} providers")
    for p, n in sorted(providers.items(), key=lambda kv: -kv[1]):
        note(f"  {p:20} {n}")
    claim("the catalogue is not empty", surface["count"] > 0, True)
    claim("count matches the rows returned", surface["count"], len(actions))

    step("the shell's own actions, as the agent receives them")
    shell_actions = [a for a in actions if a["provider"] == AGENT_PROVIDER]
    wi = max([len("ID")] + [len(a["id"]) for a in shell_actions])
    print(f"     {'ID'.ljust(wi)}  TITLE", flush=True)
    print(f"     {'-' * wi}  {'-' * 34}", flush=True)
    for a in shell_actions:
        print(f"     {a['id'].ljust(wi)}  {a['title']}", flush=True)

    step("every entry is addressed by (provider, id), never by position")
    claim("every entry carries provider, id, kind and title",
          all(isinstance(a.get(k), str) and a[k]
              for a in actions for k in ("provider", "id", "kind", "title")), True)
    addresses = [(a["provider"], a["id"]) for a in actions]
    claim("every address is unique", len(set(addresses)), len(addresses))
    claim("no entry carries a screen position",
          any(k in a for a in actions for k in ("rect", "x", "y")), False)

    step("enumerating is read-only: surveying cannot fire a side-effect")
    # `revision` is the chrome's own change counter — anything that
    # mutates chrome bumps it. If enumerating moved it, then asking
    # "what can I do here" would itself be doing something.
    before = agent.chrome()
    for query in ("", "room", "terminal"):
        agent.enumerate_actions(query)
    after = agent.chrome()
    claim("the chrome revision did not move", after["revision"], before["revision"])
    claim("no overlay opened", after["overlays"], before["overlays"])
    claim("the palette did not open", after["palette"]["visible"], False)
    agent.snapshot(os.path.join(shots, "03-agent-before.png"))
    return actions


def beat_three(quinten: Session, xaver: Session, agent: Session,
               actions: list[dict], shots: str) -> None:
    banner("3.  the agent acts, by address")

    baseline = agent.input_ops
    note(f"synthetic-input commands sent to the agent before this beat: {baseline}")

    step("pick one action out of the catalogue, by id")
    picked = next((a for a in actions if a["provider"] == AGENT_PROVIDER
                   and a["id"] == AGENT_ACTION_ID), None)
    claim(f"the catalogue offers ({AGENT_PROVIDER}, {AGENT_ACTION_ID})",
          picked is not None, True)
    if picked is None:
        raise SystemExit("the agent's chosen action is not exported")
    note(f"picked {picked['title']!r}  [{picked['kind']}]")

    step("invoke it — no click, no keystroke, no pixel")
    before_q, before_x = quinten.overlays(), xaver.overlays()
    claim("the effect is absent beforehand",
          agent.overlays()[AGENT_ACTION_EFFECT], False)
    resp = agent.invoke_action(AGENT_PROVIDER, AGENT_ACTION_ID)
    note(f"actions_invoke -> {json.dumps(resp)}")
    claim("the invocation was accepted", "answer" in resp, True)
    claim("the reply names the address that ran",
          (resp.get("answer", {}).get("provider"), resp.get("answer", {}).get("id")),
          (AGENT_PROVIDER, AGENT_ACTION_ID))
    claim("the reply names what it ran",
          resp.get("answer", {}).get("title"), picked["title"])

    step("the effect is real")
    landed = wait_for(lambda: agent.overlays()[AGENT_ACTION_EFFECT], timeout=8)
    claim("the action changed live state", bool(landed), True)
    agent.snapshot(os.path.join(shots, "04-agent-after.png"))

    step("no synthetic input was used to do it")
    claim("no keystroke, click or modifier reached the agent",
          agent.input_ops, baseline)
    note("the address (provider, id) is the entire input to the invocation")

    step("read the other two sessions back")
    # Cross-session verification of the agent's action. What is checkable
    # today is containment: the agent acting did not move quinten's or
    # xaver's screen, and did not touch the room they share.
    room = agent.active_workspace()
    claim("quinten's overlays are untouched", quinten.overlays(), before_q)
    claim("xaver's overlays are untouched", xaver.overlays(), before_x)
    claim("quinten's copy of the room is unchanged",
          (quinten.active_workspace()["name"], quinten.active_workspace()["owner"]),
          (room["name"], room["owner"]))
    claim("xaver's copy of the room is unchanged",
          (xaver.active_workspace()["name"], xaver.active_workspace()["owner"]),
          (room["name"], room["owner"]))
    # Say plainly what could not be shown, rather than dressing
    # containment up as propagation.
    note("the replicated room document carries name, owner and "
         "capabilities; window layout and chrome are per-session")
    gap("no enumerable action writes the replicated room document today, "
        "so an agent invocation cannot yet be shown *landing* in "
        "quinten's or xaver's session — only as contained to its own")


def run(quinten: Session, xaver: Session, agent: Session, shots: str) -> None:
    beat_one(quinten, xaver, agent, shots)
    actions = beat_two(agent, shots)
    beat_three(quinten, xaver, agent, actions, shots)


# --------------------------------------------------------------------


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", default=os.path.join(REPO, "target", "yc-demo"),
                    help="run directory for state, logs and screenshots")
    ap.add_argument("--shell", default=os.path.join(REPO, "target", "debug", "shell"),
                    help="path to the compositor binary")
    ap.add_argument("--size", default=None, metavar="WxH",
                    help="headless viewport (default 1280x800, or "
                         f"{RECORD_SIZE} under --record)")
    ap.add_argument("--keep", action="store_true",
                    help="leave the three sessions running afterwards, so they "
                         "can be driven by hand on camera")
    ap.add_argument("--record", nargs="?", const="", default=None, metavar="DIR",
                    help="capture all three sessions while the demo runs and "
                         "compose a 1920x1080 three-pane video "
                         "(default DIR: <out>/recording)")
    ap.add_argument("--record-size", default=RECORD_SIZE, metavar="WxH",
                    help=f"viewport each session is recorded at (default "
                         f"{RECORD_SIZE}): one video column wide, so three of "
                         f"them tile into {VIDEO_W}x{VIDEO_H} with no "
                         f"resampling. Overridden by an explicit --size.")
    ap.add_argument("--record-interval", type=float, default=0.0, metavar="S",
                    help="target seconds between snapshots of one session; 0 "
                         "means as fast as the compositor sustains, which is "
                         "about 1.2 frames/s per session at the record size")
    ap.add_argument("--record-dwell", type=float, default=2.5, metavar="S",
                    help="seconds to hold on each beat and step while "
                         "recording, so the video runs long enough to read. "
                         "Real elapsed time, really sampled — never a held "
                         "frame standing in for one")
    ap.add_argument("--record-fps", type=int, default=30, metavar="N",
                    help="container frame rate of the composed video; the "
                         "capture rate is whatever was achieved, and frames "
                         "are held, never interpolated, to fill it")
    args = ap.parse_args()

    # No fallback and no fixture: this demonstration is only meaningful
    # against the real compositor, so a missing binary is an exit, never
    # a substitute.
    if not os.path.exists(args.shell):
        print(f"no compositor at {args.shell}\n"
              f"build it first:  cargo build -p shell --bin shell", file=sys.stderr)
        return 2

    # A recording wants the size that tiles into the video 1:1; a plain
    # run wants a desktop-shaped viewport. An explicit --size beats both.
    size = args.size or (args.record_size if args.record is not None
                         else "1280x800")

    # Absolute, always. A relative --out reaches the compositor as a
    # relative XDG_CONFIG_HOME, which `dirs::config_dir()` ignores, and
    # the three sessions then persist into the real ~/.config/qos rather
    # than into this run.
    args.out = os.path.abspath(args.out)

    # Always from a clean slate. Sessions persist their workspaces, so a
    # re-run over a previous run's state starts with rooms already joined
    # and a catalogue that no longer offers `peer.connect` — the demo
    # then fails on a stale precondition rather than on anything real.
    # `--keep` is about teardown, never about reusing state.
    shutil.rmtree(args.out, ignore_errors=True)
    shots = os.path.join(args.out, "shots")
    os.makedirs(shots, exist_ok=True)

    # Unix socket paths are capped near 108 bytes, so they go in the
    # runtime dir rather than next to the rest of the run.
    runtime = os.environ.get("XDG_RUNTIME_DIR") or "/tmp"
    pid = os.getpid()
    sessions = [
        Session(name, name,
                os.path.join(args.out, name),
                os.path.join(runtime, f"qos-yc-{short}-{pid}.sock"),
                args.shell, size,
                # A private runtime dir per session, standing in for the
                # per-user one logind would give each real login.
                os.path.join(runtime, f"qyc-{short}{pid}"))
        for short, name in zip("qxa", ("quinten", "xaver", "agent"))
    ]
    quinten, xaver, agent = sessions

    print(f"compositor:    {args.shell}")
    print(f"run directory: {args.out}")

    global RECORDER
    record_dir = None
    if args.record is not None:
        record_dir = args.record or os.path.join(args.out, "recording")
        record_dir = os.path.abspath(record_dir)
        os.makedirs(record_dir, exist_ok=True)
        RECORDER = Recorder(sessions, record_dir, args.record_interval,
                            args.record_dwell)
        print(f"recording to:  {record_dir}")

    started = time.time()
    rc = 0
    try:
        for s in sessions:
            s.start()
        for s in sessions:
            s.wait_ready()
            print(f"{s.name} ready (pid {s.proc.pid}, user {s.user})")
        # Only once all three answer: a track that starts before its
        # session does is a track of a blank screen.
        if RECORDER:
            RECORDER.start()
        run(quinten, xaver, agent, shots)
    except Exception as exc:  # noqa: BLE001 — the report is the product
        FAILURES.append(f"{type(exc).__name__}: {exc}")
        import traceback
        traceback.print_exc()
    finally:
        # Before teardown either way — the capture threads talk to the
        # sessions, and `--keep` is about the sessions, not the camera.
        if RECORDER:
            RECORDER.stop()
        if args.keep:
            print("\n--keep: leaving the three sessions running.")
            for s in sessions:
                print(f"  {s.name:8} QOS_TEST_CONTROL={s.sock}  "
                      f"(pid {s.proc.pid if s.proc else '?'})")
        else:
            for s in sessions:
                s.stop()
                try:
                    os.unlink(s.sock)
                except FileNotFoundError:
                    pass

    video = None
    if RECORDER:
        RECORDER.report()
        # Composed after teardown: encoding three tracks is CPU-hungry,
        # and the sessions have nothing left to say by now.
        try:
            out_video = os.path.join(record_dir, "qos-demo.mp4")
            print(f"\ncomposing {out_video}")
            video = RECORDER.compose(out_video, args.record_fps, size)
        except Exception as exc:  # noqa: BLE001
            FAILURES.append(f"recording: {type(exc).__name__}: {exc}")
            print(f"  recording failed: {exc}", file=sys.stderr)
        for e in RECORDER.errors:
            FAILURES.append(f"recording: {e}")

    print("\n" + "=" * 70)
    print(f"elapsed: {time.time() - started:.1f}s")
    if GAPS:
        print(f"{len(GAPS)} thing(s) this run could not verify:")
        for g in GAPS:
            print(f"  - {g}")
    if FAILURES:
        rc = 1
        print(f"{len(FAILURES)} claim(s) did not hold:")
        for f in FAILURES:
            print(f"  - {f}")
    else:
        print("every claim held.")
    print(f"screenshots: {shots}")
    if video:
        print(f"video:       {video['path']}")
        print(f"             {video['width']}x{video['height']} "
              f"{video['codec']} {video['fps']}  "
              f"{video['duration']:.2f}s  {video['frames']} frames  "
              f"{video['bytes'] / 1024 / 1024:.1f} MiB")
        print(f"             captured at {RECORDER.rate():.2f} frames/s "
              f"per session")
    return rc


if __name__ == "__main__":
    sys.exit(main())
