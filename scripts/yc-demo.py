#!/usr/bin/env python3
"""Three qOS sessions in one room, and an agent that drives one of them.

Starts three real compositors — separate state directories, separate
libp2p keypairs, separate user identities, separate control sockets:

  ada  hosts the room and owns it
  bob    joins it by invite
  agent    joins it too, and from then on is driven *only* by the action
           API: semantic invocation by `(provider, id)`, never by a
           click, a keystroke, or a pixel

Three beats, about a minute:

  1. one room, three sessions   — proven by reading ada's room back
                                  out of bob's and the agent's own state
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

Runs against target/demo/shell:

    cargo build --profile demo -p shell --bin shell --bin qos-control

There is no mock and no fixture path: if the binary is missing the
script exits rather than simulating it.

Build it optimised. `snapshot` re-renders the scene and PNG-encodes it
on the compositor's own event loop, so the session is blocked for every
millisecond of it — and an unoptimised build spends ~1.4 s there per
frame at 1920x1080, which is a recording nobody can watch.

`--release` is not the profile to use, and not because of speed: the
`QOS_TEST_CONTROL` socket this script drives the compositor through is a
complete lock-screen bypass, so a shipped build deliberately does not
contain the code that reads it. A `--release` shell binds no socket and
never says why. `demo` is the release profile with that one gate left
open, and nothing about `--release` changes.
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
        # A compositor that is still running but never bound the socket is
        # almost always a `--release` build: `compositor::dev_env` gates
        # QOS_TEST_CONTROL on `cfg!(debug_assertions)`, so a shipped binary
        # has no such code and logs nothing about it. Say so, rather than
        # leaving the next person to read a healthy log for an hour.
        alive = self.proc is not None and self.proc.poll() is None
        raise RuntimeError(
            f"{self.name} never answered on {self.sock}"
            + (f"\n  the process is still alive, so it started and did not bind"
               f"\n  the control socket. Is {self.binary} a --release build?"
               f"\n  QOS_TEST_CONTROL is compiled out of release builds by"
               f"\n  design; build --profile demo instead."
               if alive else f"\n  see {self.log_path}"))

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
# One session is on camera at a time and the video cuts between them,
# the way the narrative moves: to whoever is acting, and to a peer at
# the moment the point is that the peer *sees* it. Three panes at once
# had to shrink every session to a third of the width, which clipped
# the top bar and overflowed the tab strip, and it split the sample
# budget three ways to show two screens where nothing was happening.
#
# Cutting pays for itself twice: each session renders full-frame at
# 1:1, and the only session being sampled is the one you are looking
# at, so the whole capture budget goes to the pixels on screen.
#
# Frames are written at their true timestamps — each one shows from
# when it was taken until the next was, so the video's clock is the
# demo's clock. Nothing is interpolated, and with a single track there
# is no held frame either: no frame in the file was shown for longer
# than the session held it. Where the video needs to be longer the
# demo is given more real time to be longer in (`--record-dwell`,
# `--record-cut-dwell`), never more frames than it took.
VIDEO_W, VIDEO_H = 1920, 1080
# What each session is recorded at: the video's own size, so a frame
# goes in untouched. Resampling is the difference between a readable
# screen and a blurred one, and there is no reason to pay it here.
RECORD_SIZE = f"{VIDEO_W}x{VIDEO_H}"

# The lower third. The name of the session and what it is, over a scrim
# dark enough to stay legible against whatever it is sitting on — and
# nothing else. The transcript belongs in the terminal; on the video it
# was a wall of text nobody reads while a screen is moving underneath.
BAND_H = 66
BAND_SCRIM = "black@0.72"
BAND_PAD_X = 52

BG = "0x0B0D12"       # the shell's own backdrop, for any letterboxing
FG = "0xE9ECF2"

# What each session is, in the video. Roles, not decoration: with one
# screen on at a time the label is the only thing telling you whose
# screen you are on, and the third one's whole point is that it is
# driven differently from the other two.
ROLES = {
    "ada": "hosts the room",
    "bob": "joined by invite",
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
# every reply, and the waits it depends on start timing out. Only one
# session is sampled now, but it is also the session the demo is
# driving, so the yield matters more rather than less.
MIN_CAPTURE_GAP_S = 0.04
# …and on top of the floor, this share of whatever the last snapshot
# actually cost. A fixed gap is the wrong shape for a cost that moves by
# a factor of twenty between an unoptimised build and an optimised one
# with a warm renderer: too small and the demo queues behind its own
# camera, too large and a fast compositor sits idle between frames.
# A share holds the duty cycle steady at either end.
CAPTURE_YIELD = 0.25
# What one 1920x1080 snapshot costs an idle machine with this build, in
# ms. Not a threshold anything is enforced against — the recording is
# whatever it turns out to be — only the number the report compares
# against so a contended run says so instead of looking like a slow qOS.
SNAPSHOT_BUDGET_MS = 150.0


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


class Recorder:
    """Timestamped frames pulled from whichever session is on camera.

    The capture thread is a reader: `snapshot` re-paints and writes a
    PNG, and never enters the input path, so the agent's "no synthetic
    input" claim is as true during a recording as without one —
    `input_ops` is untouched by anything in here.

    One thread, one track. `cut_to` moves the camera; every frame
    records which session it came from, so the label on screen is
    derived from the pixels rather than from when a cut was ordered.
    """

    def __init__(self, sessions: list[Session], directory: str,
                 interval: float, dwell: float = 0.0,
                 cut_dwell: float = 0.0) -> None:
        self.sessions = sessions
        self.by_name = {s.name: s for s in sessions}
        self.dir = directory
        self.interval = interval
        self.dwell_s = dwell
        self.cut_dwell_s = cut_dwell
        # (taken_at, relative path, session name) in capture order.
        self.frames: list[tuple[float, str, str]] = []
        # How long each snapshot blocked its session. The honest measure
        # of what the camera costs, reported rather than estimated.
        self.blocked: list[float] = []
        self.errors: list[str] = []
        self._failed: set[str] = set()
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None
        self._live: Session | None = None
        self._lock = threading.Lock()
        self.started_at = 0.0
        self.stopped_at = 0.0

    # -- capture -----------------------------------------------------

    def start(self, first: str) -> None:
        os.makedirs(os.path.join(self.dir, "frames"), exist_ok=True)
        self._live = self.by_name[first]
        self.started_at = time.time()
        self._thread = threading.Thread(target=self._capture, name="capture",
                                        daemon=True)
        self._thread.start()

    def _capture(self) -> None:
        index = 0
        while not self._stop.is_set():
            with self._lock:
                s = self._live
            if s is None:
                self._stop.wait(0.02)
                continue
            began = time.time()
            rel = f"frames/{index:06d}.png"
            try:
                s.snapshot(os.path.join(self.dir, rel))
            except Exception as exc:  # noqa: BLE001
                # A session going quiet costs its own frames and nothing
                # else: the camera keeps rolling, and a later cut to a
                # healthy session picks straight back up. Reported once
                # per session rather than once per attempt.
                if s.name not in self._failed:
                    self._failed.add(s.name)
                    self.errors.append(
                        f"{s.name}: capture failed at frame {index}: "
                        f"{type(exc).__name__}: {exc}")
                self._stop.wait(0.25)
                continue
            took = time.time() - began
            self.blocked.append(took)
            self.frames.append((began, rel, s.name))
            index += 1
            self._stop.wait(max(MIN_CAPTURE_GAP_S, CAPTURE_YIELD * took,
                                self.interval - took))

    # -- the cut -----------------------------------------------------

    def cut_to(self, name: str, dwell: float | None = None) -> None:
        """Put one session on camera, and hold there long enough to read
        it before the demo moves on.

        Cutting to the session already on camera is a no-op, so the beat
        functions can name their subject without inserting a pause that
        the narrative did not ask for.
        """
        with self._lock:
            if self._live is not None and self._live.name == name:
                return
            self._live = self.by_name[name]
        hold = self.cut_dwell_s if dwell is None else dwell
        if hold > 0:
            time.sleep(hold)

    def dwell(self) -> None:
        """Hold on the moment just reached.

        The one honest way to make the video longer. Every second spent
        here is a second the demo really took, sampled like any other —
        as against holding a frame on screen for longer than the session
        held it, which would be inventing time the run did not have.
        """
        if self.dwell_s > 0:
            time.sleep(self.dwell_s)

    def stop(self) -> None:
        self._stop.set()
        if self._thread:
            self._thread.join(timeout=30)
            self._thread = None
        self.stopped_at = time.time()

    # -- what actually happened --------------------------------------

    def rate(self) -> float:
        """Frames per second, as achieved. One track, so this is also
        the rate of whatever is on screen at any moment."""
        span = max(1e-6, self.stopped_at - self.started_at)
        return len(self.frames) / span

    def snapshot_ms(self) -> tuple[float, float]:
        """Mean and worst time one snapshot blocked its session, in ms."""
        if not self.blocked:
            return 0.0, 0.0
        return (sum(self.blocked) / len(self.blocked) * 1000.0,
                max(self.blocked) * 1000.0)

    def report(self) -> None:
        span = self.stopped_at - self.started_at
        per: dict[str, int] = {}
        for _, _, name in self.frames:
            per[name] = per.get(name, 0) + 1
        mean_ms, worst_ms = self.snapshot_ms()
        print(f"\ncaptured over {span:.1f}s of demo: {len(self.frames)} frames, "
              f"{self.rate():.2f} frames/s")
        print(f"  snapshot blocked its session {mean_ms:.0f} ms on average "
              f"(worst {worst_ms:.0f} ms)")
        # The frame rate of this video *is* the compositor's snapshot
        # speed, so a recording made on a busy machine measures the rest
        # of the machine. Worth saying outright: the number above is
        # otherwise indistinguishable from qOS being slow.
        if mean_ms > SNAPSHOT_BUDGET_MS:
            print(f"  ! that is well over the ~{SNAPSHOT_BUDGET_MS:.0f} ms an "
                  f"idle machine takes at {VIDEO_W}x{VIDEO_H}. Parallel "
                  f"builds or other compositors are the usual cause; the "
                  f"frame rate above is what they cost this recording.")
        print("  time on camera:")
        for s in self.sessions:
            n = per.get(s.name, 0)
            print(f"    {s.name:8} {n:5d} frames  "
                  f"{n / max(1e-6, self.rate()):.1f}s")
        for e in self.errors:
            print(f"  ! {e}")

    # -- composition -------------------------------------------------

    def _window(self) -> tuple[float, float]:
        if not self.frames:
            raise RuntimeError("no frames were captured")
        return self.frames[0][0], self.frames[-1][0]

    def _concat_list(self, t_start: float, t_end: float,
                     fps: int) -> tuple[str, float]:
        """The single track, on the demo's own clock.

        Each frame is shown from when it was taken until the next one
        was — so the video runs in real time and is simply as dense as
        the camera managed. With one track there is nothing to pad
        against, so no frame in the file is ever repeated.
        """
        frames = self.frames
        if not frames:
            raise RuntimeError("no frames to compose")
        tick = 1.0 / fps
        spans = []
        for i, (t, _rel, _name) in enumerate(frames):
            begins = t_start if i == 0 else t
            # The last frame was the live state until the camera
            # stopped; it is shown for one tick and the video ends.
            ends = frames[i + 1][0] if i + 1 < len(frames) else t_end + tick
            spans.append(max(tick, ends - begins))
        lines = ["ffconcat version 1.0"]
        for (_t, rel, _name), seconds in zip(frames, spans):
            lines.append(f"file '{rel}'")
            lines.append(f"duration {seconds:.4f}")
        with open(os.path.join(self.dir, "track.txt"), "w") as f:
            f.write("\n".join(lines) + "\n")
        return "track.txt", sum(spans)

    def _label_spans(self, t_start: float, t_end: float,
                     fps: int) -> list[tuple[str, float, float]]:
        """When each session's name is on screen.

        Taken from the frames themselves rather than from the cut
        timestamps, so the label always names the session whose pixels
        are showing — not the one the camera had just been asked for.
        """
        spans: list[tuple[str, float, float]] = []
        for i, (t, _rel, name) in enumerate(self.frames):
            begins = (t_start if i == 0 else t) - t_start
            ends = ((self.frames[i + 1][0] if i + 1 < len(self.frames)
                     else t_end + 1.0 / fps) - t_start)
            if spans and spans[-1][0] == name:
                spans[-1] = (name, spans[-1][1], ends)
            else:
                spans.append((name, begins, ends))
        return [s for s in spans if s[2] - s[1] >= 1.0 / fps]

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

    def compose(self, out_path: str, fps: int, size: str) -> dict:
        if shutil.which("ffmpeg") is None or shutil.which("ffprobe") is None:
            raise RuntimeError("ffmpeg and ffprobe are needed to compose the "
                               "video; the frames are still on disk")
        bold = _pick_font(FONTS_BOLD)
        t_start, t_end = self._window()
        # Nothing to time a video against if the demo never got going.
        if t_end - t_start < 1.0:
            raise RuntimeError(
                f"only {t_end - t_start:.2f}s of capture; too little to compose")

        source_w, source_h = (int(v) for v in size.lower().split("x"))
        rel_track, _seconds = self._concat_list(t_start, t_end, fps)

        chain = [f"fps={fps}"]
        # No scale filter at all when the session already renders at the
        # video's size, which is what `--record-size` picks by default.
        # Resampling a 1:1 frame would cost exactly the legibility that
        # recording full-frame was for.
        if (source_w, source_h) != (VIDEO_W, VIDEO_H):
            k = min(VIDEO_W / source_w, VIDEO_H / source_h)
            w, h = _even(source_w * k), _even(source_h * k)
            print(f"  {size} is not {VIDEO_W}x{VIDEO_H}; frames are resampled "
                  f"to {w}x{h} and letterboxed. Record at {RECORD_SIZE} "
                  f"for 1:1.")
            chain.append(f"scale={w}:{h}:flags=lanczos")
            chain.append(f"pad={VIDEO_W}:{VIDEO_H}:{(VIDEO_W - w) // 2}:"
                         f"{(VIDEO_H - h) // 2}:color={BG}")
        chain.append("format=rgb24")
        # The scrim is always up, so the band does not blink between
        # cuts — only the name inside it changes.
        chain.append(f"drawbox=x=0:y={VIDEO_H - BAND_H}:w={VIDEO_W}:"
                     f"h={BAND_H}:color={BAND_SCRIM}:t=fill")

        def label(rel: str, enable: str) -> str:
            return "drawtext=" + ":".join([
                f"fontfile={bold}", f"textfile={rel}", f"fontcolor={FG}",
                "fontsize=27", f"x={BAND_PAD_X}",
                f"y={VIDEO_H - BAND_H}+({BAND_H}-text_h)/2",
                f"enable='{enable}'",
            ])

        for i, (name, a, b) in enumerate(self._label_spans(t_start, t_end, fps)):
            text = f"{name} · {ROLES[name]}" if name in ROLES else name
            chain.append(label(self._caption(f"n{i:03d}", text),
                               f"between(t,{a:.3f},{b:.3f})"))
        chain.append("format=yuv420p")

        graph = "[0:v]" + ",".join(chain) + "[v]"
        with open(os.path.join(self.dir, "filter.txt"), "w") as f:
            f.write(graph + "\n")

        cmd = ["ffmpeg", "-y", "-v", "error",
               "-f", "concat", "-safe", "0", "-i", rel_track,
               "-filter_complex", graph, "-map", "[v]",
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

# Set by `--record`. The transcript is the demo's narration, and it stays
# in the terminal: on the video the beats and claims were a wall of text
# competing with the screen underneath, so what the recording takes from
# these calls is timing — how long to hold — and nothing else.
RECORDER: Recorder | None = None


def cut(name: str) -> None:
    """Put `name` on camera for what follows.

    Cuts follow the narrative rather than the clock: to whoever is
    acting, and to a peer at the moment the point is that the peer sees
    it. Printed either way, so the transcript documents the intended
    order of shots even on a run that is not being recorded.
    """
    print(f"\n  [ on camera: {name} ]", flush=True)
    if RECORDER:
        RECORDER.cut_to(name)


def banner(title: str) -> None:
    print(f"\n{RULE}\n  {title}\n{RULE}", flush=True)
    if RECORDER:
        RECORDER.dwell()


def step(text: str) -> None:
    print(f"\n  {text}", flush=True)
    if RECORDER:
        RECORDER.dwell()


def claim(description: str, actual, expected) -> None:
    good = actual == expected
    print(f"    [{'PASS' if good else 'FAIL'}] {description}", flush=True)
    if not good:
        print(f"           expected {expected!r}", flush=True)
        print(f"           observed {actual!r}", flush=True)
        FAILURES.append(description)


def gap(description: str) -> None:
    """Something this run could not verify. Its own mark, so it can never
    be mistaken for a pass."""
    print(f"    [GAP ] {description}", flush=True)
    GAPS.append(description)


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


def beat_one(ada: Session, bob: Session, agent: Session, shots: str) -> dict:
    banner("1.  one room, three sessions")
    # Open on the host. The room is his, and everything in this beat is
    # something he hands out or something a guest reads back from him.
    cut("ada")

    ids = {s.name: peer_id_of(s) for s in (ada, bob, agent)}
    for s in (ada, bob, agent):
        note(f"{s.name:8} pid {s.proc.pid:<8} user {s.user:<8} peer {ids[s.name]}")
    claim("three separate processes",
          len({s.proc.pid for s in (ada, bob, agent)}), 3)
    claim("three separate libp2p identities", len(set(ids.values())), 3)

    host_ws = ada.active_workspace()
    note(f"ada's room {host_ws['room']}  named {host_ws['name']!r}")
    claim("ada owns his room", host_ws["owner"], ada.user)

    step("bob joins by invite")
    # Minting is ada's: the palette opens on his screen and the
    # invite card comes up there.
    token = mint_invite(ada)
    claim("ada minted an invite token", bool(token), True)
    if not token:
        raise SystemExit("no invite token: the rest cannot run")
    # Redeeming is bob's, and it is his screen that changes.
    cut("bob")
    claim("bob joined", join_room(bob, token, by_action_api=False), True)

    step("the agent joins by invite")
    cut("ada")
    token2 = mint_invite(ada)
    claim("ada minted a second token", bool(token2), True)
    if not token2:
        raise SystemExit("no second token: the agent cannot join")
    cut("agent")
    claim("the agent joined", join_room(agent, token2, by_action_api=True), True)
    # The connect *prompt* was opened by address; the token itself had to
    # be typed, because the control socket has no semantic join op.
    gap("joining has no semantic op — the invite token had to be typed "
        "(only the prompt was opened by address)")

    step("read ada's room back out of the other two sessions")
    # The verification that matters: neither of these is ada's own
    # state. Each guest independently reports the room it now holds —
    # so the camera sits on the guest doing the reporting, and the
    # claims are grouped by whose screen you are looking at.
    cut("bob")
    claim("bob's own state holds ada's room",
          bob.active_workspace()["room"], host_ws["room"])
    claim("bob's own copy names ada as owner",
          bob.active_workspace()["owner"], ada.user)
    claim("bob's own copy carries ada's name for the room",
          bob.active_workspace()["name"], host_ws["name"])

    cut("agent")
    claim("the agent's own state holds ada's room",
          agent.active_workspace()["room"], host_ws["room"])
    claim("the agent's own copy names ada as owner",
          agent.active_workspace()["owner"], ada.user)
    claim("the agent's own copy carries it too",
          agent.active_workspace()["name"], host_ws["name"])
    ada.snapshot(os.path.join(shots, "01-ada.png"))
    bob.snapshot(os.path.join(shots, "02-bob.png"))
    return host_ws


def beat_two(agent: Session, shots: str) -> list[dict]:
    banner("2.  the agent sees a typed, addressable surface")
    # The whole beat is the agent's, and the screen deliberately does
    # not move: what is being shown is that surveying the surface
    # changes nothing on it.
    cut("agent")

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


def beat_three(ada: Session, bob: Session, agent: Session,
               actions: list[dict], shots: str) -> None:
    banner("3.  the agent acts, by address")
    cut("agent")

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
    before_q, before_x = ada.overlays(), bob.overlays()
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
    # today is containment: the agent acting did not move ada's or
    # bob's screen, and did not touch the room they share. The camera
    # goes to each of them in turn, because a screen that did *not*
    # change is only worth claiming if you are shown it.
    room = agent.active_workspace()
    cut("ada")
    claim("ada's overlays are untouched", ada.overlays(), before_q)
    claim("ada's copy of the room is unchanged",
          (ada.active_workspace()["name"], ada.active_workspace()["owner"]),
          (room["name"], room["owner"]))

    cut("bob")
    claim("bob's overlays are untouched", bob.overlays(), before_x)
    claim("bob's copy of the room is unchanged",
          (bob.active_workspace()["name"], bob.active_workspace()["owner"]),
          (room["name"], room["owner"]))
    # Say plainly what could not be shown, rather than dressing
    # containment up as propagation.
    note("the replicated room document carries name, owner and "
         "capabilities; window layout and chrome are per-session")
    gap("no enumerable action writes the replicated room document today, "
        "so an agent invocation cannot yet be shown *landing* in "
        "ada's or bob's session — only as contained to its own")


def run(ada: Session, bob: Session, agent: Session, shots: str) -> None:
    beat_one(ada, bob, agent, shots)
    actions = beat_two(agent, shots)
    beat_three(ada, bob, agent, actions, shots)


# --------------------------------------------------------------------


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", default=os.path.join(REPO, "target", "yc-demo"),
                    help="run directory for state, logs and screenshots")
    ap.add_argument("--shell",
                    default=os.path.join(REPO, "target", "demo", "shell"),
                    help="path to the compositor binary; must be a build that "
                         "still honours QOS_TEST_CONTROL (any profile with "
                         "debug-assertions on — `demo` or `dev`, never "
                         "`--release`)")
    ap.add_argument("--size", default=None, metavar="WxH",
                    help="headless viewport (default 1280x800, or "
                         f"{RECORD_SIZE} under --record)")
    ap.add_argument("--keep", action="store_true",
                    help="leave the three sessions running afterwards, so they "
                         "can be driven by hand on camera")
    ap.add_argument("--record", nargs="?", const="", default=None, metavar="DIR",
                    help="capture whichever session is on camera while the "
                         f"demo runs and compose a {VIDEO_W}x{VIDEO_H} video "
                         "(default DIR: <out>/recording)")
    ap.add_argument("--record-size", default=RECORD_SIZE, metavar="WxH",
                    help=f"viewport each session is recorded at (default "
                         f"{RECORD_SIZE}): the video's own size, so a frame "
                         f"goes in with nothing resampled. Overridden by an "
                         f"explicit --size.")
    ap.add_argument("--record-interval", type=float, default=0.0, metavar="S",
                    help="target seconds between snapshots; 0 means as fast "
                         "as the compositor sustains")
    ap.add_argument("--record-dwell", type=float, default=1.9, metavar="S",
                    help="seconds to hold on each beat and step while "
                         "recording, so the video runs long enough to read. "
                         "Real elapsed time, really sampled — never a held "
                         "frame standing in for one")
    ap.add_argument("--record-cut-dwell", type=float, default=3.0, metavar="S",
                    help="seconds to hold after cutting to a session, so the "
                         "screen you have just been moved to registers "
                         "before the demo carries on")
    ap.add_argument("--record-fps", type=int, default=30, metavar="N",
                    help="container frame rate of the composed video; the "
                         "capture rate is whatever was achieved, and frames "
                         "are held, never interpolated, to fill it")
    args = ap.parse_args()

    # No fallback and no fixture: this demonstration is only meaningful
    # against the real compositor, so a missing binary is an exit, never
    # a substitute.
    if not os.path.exists(args.shell):
        print(f"no compositor at {args.shell}\nbuild it first:  "
              f"cargo build --profile demo -p shell --bin shell --bin qos-control",
              file=sys.stderr)
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
        for short, name in zip("qxa", ("ada", "bob", "agent"))
    ]
    ada, bob, agent = sessions

    print(f"compositor:    {args.shell}")
    print(f"run directory: {args.out}")

    global RECORDER
    record_dir = None
    if args.record is not None:
        record_dir = args.record or os.path.join(args.out, "recording")
        record_dir = os.path.abspath(record_dir)
        os.makedirs(record_dir, exist_ok=True)
        RECORDER = Recorder(sessions, record_dir, args.record_interval,
                            args.record_dwell, args.record_cut_dwell)
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
        # session does is a track of a blank screen. Opens on the host,
        # which is where the first beat opens too.
        if RECORDER:
            RECORDER.start(ada.name)
        run(ada, bob, agent, shots)
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
        mean_ms, worst_ms = RECORDER.snapshot_ms()
        print(f"             captured at {RECORDER.rate():.2f} frames/s; "
              f"one snapshot cost {mean_ms:.0f} ms mean / {worst_ms:.0f} ms worst")
    return rc


if __name__ == "__main__":
    sys.exit(main())
