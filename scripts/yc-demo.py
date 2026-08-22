#!/usr/bin/env python3
"""Three qOS sessions in one room, and an agent that drives one of them.

Starts three real compositors — separate state directories, separate
libp2p keypairs, separate user identities, separate control sockets:

  ada  hosts the room and owns it
  bob    joins it by invite
  agent    joins it too, and from then on is driven *only* by the action
           API: semantic invocation by `(provider, id)`, never by a
           click, a keystroke, or a pixel

The storyboard, in order, about a minute:

  1. an empty desktop, and Files opened on it
  2. ada invites bob
  3. ada invites the agent — a capability principal, not a peer
  4. both have joined, and each session can see the others
  5. "word processor" — a machine that does not have it, fetching it
  6. cut to bob: the window ada opened is open for him too
  7. ada writes, and bob's screen is asked what it holds
  8. an image into the document, through the OS file chooser
  9. the agent acts by address, and the other two read the result back

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
import zlib

# `reaper` lives beside the other headless drivers rather than on the
# path, so it is imported by location — after `sys`, which the line
# doing the importing needs.
sys.path.insert(
    0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "headless-examples"))
from reaper import die_with_parent          # noqa: E402

# Set from `--keep`. Off means the kernel reaps a compositor with this
# script, however this script dies.
KEEP_SESSIONS = False

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# The action the agent invokes, as a stable `(provider, id)` address from
# the shell's own action provider — the same registry that feeds the
# command palette, reached with no palette and no keystroke.
AGENT_PROVIDER = "actions"
AGENT_ACTION_ID = "room.activity.open"
# Its effect, as a key under `chrome.overlays` in `state_dump`.
AGENT_ACTION_EFFECT = "room_activity"

# The provider that launches installed applications, and the two apps the
# storyboard opens. An app is started by its own id through the same
# registry that feeds the palette, so the demo drives the production
# launch path rather than a test entry point.
APPLICATIONS = "applications"
FILES_APP = "dev.qos.file-explorer"
WORD_PROCESSOR = "dev.qos.word-processor"

# Machine state, deliberately *not* isolated per session: without these
# two a session enumerates zero applications and every launch fails with
# "no bundle was found". Documents and identities stay per-session; the
# installed-app store and the runtime bundle are the machine's.
MACHINE_APPS = os.path.expanduser("~/.local/share/qos-apps")
RUNTIME_BUNDLES = os.path.expanduser("~/.local/share/qos-runtime")
# A registry holding the word processor built from *this* tree. The
# machine's installed payload is a static binary from an earlier build
# and cannot exercise anything that has changed in `ui` since, so a run
# against it would be filming last month's app.
LIVE_APPS = os.path.join(REPO, "target", "qos-apps-live")

# The capability principal ada grants in beat 3. An agent is not a
# peer: it is a subject in the permission model, and the grant is scoped
# to one app rather than to the workspace.
AGENT_SUBJECT = "agent:scribe"
AGENT_SCOPE = f"app:{WORD_PROCESSOR}"

# What ada types into the document, and what bob's session is then
# asked whether it holds.
TYPED_TEXT = "the same document on both machines"

# How long to wait for a change to cross between sessions.
SYNC_TIMEOUT_S = 25.0
# A document crossing is a longer chain than a room-index entry: publish,
# gossip, drain on the far side, apply, paint. Given its own budget so a
# slow hop is not reported as a failure to converge.
DOC_SYNC_TIMEOUT_S = 60.0
# An app launch is a sandbox spawn, a Wayland connection, a toolkit
# starting up and a first frame. Generous because none of that is qOS
# being slow when the machine is busy: at a load average near a hundred
# this has been measured past two minutes, and a launch reported as
# failed because the harness gave up is the worst kind of wrong answer.
LAUNCH_TIMEOUT_S = 240.0
# What one `snapshot` is allowed to block its session for. Deliberately
# far above what it costs: the op re-renders and encodes on the
# compositor's own loop, and on a machine that is busy compiling
# something else an unoptimised build can sit there for a minute. A tight
# timeout here reports a contended box as a broken compositor.
SNAPSHOT_TIMEOUT_S = 240.0


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
                 size: str, runtime_dir: str, home: str, apps_dir: str,
                 tools: dict[str, str] | None = None):
        self.name = name
        self.user = user
        self.dir = run_dir
        self.sock = sock
        self.binary = binary
        self.size = size
        # This session's own home. The file chooser and the palette's file
        # results read it, so pointing it at a staged directory is what
        # keeps the demo off whoever's machine is filming it.
        self.home = home
        # The installed-app store this session sees. Per-session because
        # "this machine does not have the word processor" has to be a fact
        # about a real store rather than an assertion about one.
        self.apps_dir = apps_dir
        # Paths to the packaging chain, when it has been built. Absent is
        # a supported state: the beat that needs it says so and moves on.
        self.tools = tools or {}
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

    @property
    def collab_json_sock(self) -> str:
        """The broker's legacy JSON listener.

        One broker serves both sockets (`collab_broker::spawn_full`, wired
        at `native.rs:1788`), so a JSON connection announcing the same
        `(app, room, record)` address a packaged app announces lands in
        that app's channel — even though the app itself reached the same
        broker over capnp. That is what lets this script ask a *running
        app* for the document it holds without being able to see its
        screen.
        """
        return os.path.join(self.runtime_dir, "qos", "services",
                            "collab-json.sock")

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
            # A home of this session's own, staged with the demo's files.
            HOME=self.home,
            # The two stores an app launch cannot do without. The install
            # root is this session's; the runtime bundle is the machine's,
            # because every packaged app pins it and none of them ship it.
            QOS_APPS_DATA_DIR=self.apps_dir,
            QOS_RUNTIME_BUNDLE_DIR=RUNTIME_BUNDLES,
            # Where the catalogue looks for registries, and where it caches
            # what it fetches. Per-session, so an install in one session is
            # not quietly an install in another.
            QOS_STORE_CONFIG_DIR=os.path.join(self.dir, "store-config"),
            QOS_STORE_CACHE_DIR=os.path.join(self.dir, "store-cache"),
        )
        # The packaging chain, when it is on disk. Passed by path rather
        # than found on PATH so the run measures the binaries built for it.
        for var, path in self.tools.items():
            env[var] = path
        os.makedirs(os.path.join(self.dir, "store-config"), exist_ok=True)
        os.makedirs(os.path.join(self.dir, "store-cache"), exist_ok=True)
        self._log = open(self.log_path, "w")
        self.proc = subprocess.Popen(
            [self.binary, "--mode", "native", "--backend", "headless",
             "--enable-peer-sync"],
            cwd=REPO, env=env, stdout=self._log, stderr=subprocess.STDOUT,
            # `--keep` genuinely keeps: without a session of its own a
            # child dies with this process's group the moment the script
            # returns, which made "left running" a message printed about
            # three processes that were already gone.
            #
            # Every other run asks the kernel to kill the child when this
            # process dies. `atexit` cannot cover that: a harness that
            # hangs is stopped with SIGKILL, and nothing in userspace runs
            # afterwards. Eleven compositors were once found still
            # spinning at 80% CPU each, the oldest 47 minutes after the
            # run that spawned it — most of a machine, held by runs that
            # had already been abandoned.
            start_new_session=KEEP_SESSIONS,
            preexec_fn=die_with_parent(enabled=not KEEP_SESSIONS),
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

    def peers(self) -> dict:
        """The peers this session would paint, off the same `PresenceCtx`
        the overlay draws from.

        Read rather than eyeballed on purpose: "no peer arrived" and "a
        peer arrived and was not drawn" make the same picture, and a
        harness that cannot tell those apart has not shown either.
        """
        return ok(call(self.sock, {"op": "state_dump"}))["state"].get("peers") or {}

    def foreign_frames(self) -> list[dict]:
        """Per-surface frame counters for every client window.

        `frame_seq > 0` is the ground truth behind "the app opened": a
        placeholder tile looks like a window and never presents.
        """
        r = call(self.sock, {"op": "foreign_frames"})
        if "foreign_frames" in r:
            return r["foreign_frames"].get("frames", [])
        return r.get("answer", r.get("frames", [])) or []

    def groups_here(self, tree: dict | None = None) -> dict:
        """Every window group in this session's active workspace.

        Takes an already-read tree when the caller has one. These run
        inside polling loops, and a `state_dump` blocks the compositor
        it is asked of — re-reading the tree per workspace would have
        the demo competing with its own camera for every reply.
        """
        tree = tree if tree is not None else self.tree()
        out: dict[str, dict] = {}
        for ws in tree["workspaces"]:
            if ws["id"] != tree["active"]:
                continue
            for strip in ws.get("strips", []):
                for group in strip.get("window_groups", []):
                    out[str(group.get("id"))] = {
                        "active_tab": group.get("active_tab"),
                        "tabs": [t.get("title") for t in group.get("tabs", [])],
                        "tab_ids": [str(t.get("id")) for t in group.get("tabs", [])],
                        "strip": str(strip.get("id")),
                    }
        return out

    def tabs_here(self, tree: dict | None = None) -> dict:
        """Every tab in this session's active workspace, by tab id.

        Keyed on the tab rather than the group because the placement rule
        tabs a new window into the focused group when it is already
        tabbed — so a new window does not always mean a new group.
        """
        out: dict[str, dict] = {}
        for gid, g in self.groups_here(tree).items():
            for i, t in enumerate(g["tab_ids"]):
                out[t] = {"group": gid, "strip": g["strip"],
                          "title": g["tabs"][i] if i < len(g["tabs"]) else None}
        return out

    def view_state(self) -> dict:
        """Where this person is looking. Which windows exist is the
        workspace; which one is in front of you is you, and the second
        must not travel with the first."""
        tree = self.tree()
        return {
            "active": tree.get("active"),
            "active_strip": tree.get("active_strip"),
            "active_group": tree.get("active_group"),
            "active_tab_per_group": {g: v["active_tab"]
                                     for g, v in self.groups_here(tree).items()},
        }

    def targets(self) -> list[dict]:
        """The compositor's own widget registry: every addressable piece
        of chrome with its rect. A client's insides are not in here —
        this is the shell's tree, which is exactly what makes the OS file
        chooser drivable and an app's toolbar not."""
        r = call(self.sock, {"op": "list_targets"})
        for v in (r.values() if isinstance(r, dict) else []):
            if isinstance(v, list):
                return v
            if isinstance(v, dict):
                for vv in v.values():
                    if isinstance(vv, list):
                        return vv
        return []

    def named_rect(self, name: str) -> list | None:
        for t in self.targets():
            if t.get("name") == name:
                rect = t.get("rect")
                if rect and (rect[2] > 0 and rect[3] > 0):
                    return rect
        return None

    def target_names(self) -> list[str]:
        return [t.get("name", "") for t in self.targets()]

    def check_action(self, actor: str, app: str, action: str,
                     verb: str = "write") -> dict:
        """Ask the in-app action gate whether `actor` may invoke this.

        Read-only, and the production gate: this is the same check the
        broker runs, asked without performing anything.
        """
        return call(self.sock, {"op": "check_action", "actor": actor,
                                "app": app, "action": action, "verb": verb})

    def grant(self, subject: str, scope: str, verb: str) -> dict:
        """Grant a capability, through `Workspace::grant_as`.

        The authority check is the production one — it refuses a grantor
        who neither owns the room nor holds admin over it — so a refusal
        here is the model working, not the harness failing.
        """
        return call(self.sock, {"op": "grant", "subject": subject,
                                "scope": scope, "verb": verb})

    # -- synthetic input, continued (all counted) --------------------

    def click_at(self, x: float, y: float) -> None:
        self.input_ops += 1
        ok(call(self.sock, {"op": "click_at", "x": x, "y": y}))

    def click_named(self, name: str) -> dict:
        self.input_ops += 1
        return call(self.sock, {"op": "click_named", "name": name})

    def pointer_move(self, x: float, y: float) -> None:
        self.input_ops += 1
        ok(call(self.sock, {"op": "pointer_move", "x": x, "y": y}))

    def place_next_window(self, app_id: str) -> dict:
        """Note that the next window announcing `app_id` opens in its own
        group. Two clients are only on screen at once when they are in
        separate groups, and that is the precondition for a scene with
        Files beside the document."""
        return call(self.sock, {"op": "place_next_window", "app_id": app_id})

    def set_group_width(self, permille: int) -> dict:
        """Give every group in the strip the same share of it. A group
        takes about three quarters by default, so two never fit."""
        return call(self.sock, {"op": "set_group_width", "permille": permille})

    # -- launching ---------------------------------------------------

    def launch(self, app_id: str, timeout: float = LAUNCH_TIMEOUT_S) -> dict | None:
        """Start an app by its id and wait for it to present a frame.

        The surfaces that already existed are excluded, because every app
        announces the same wayland app-id: a launch that opened nothing
        is otherwise indistinguishable from one that worked, the previous
        window still being on screen and still painting.
        """
        before = {f.get("surface_id") for f in self.foreign_frames()}
        resp = call(self.sock, {"op": "actions_invoke",
                                "provider": APPLICATIONS, "id": app_id})
        if "err" in resp:
            note(f"{self.name}: launching {app_id} was refused — "
                 f"{json.dumps(resp['err'])[:160]}")
            return None
        deadline = time.time() + timeout
        while time.time() < deadline:
            for f in self.foreign_frames():
                if f.get("surface_id") not in before and f.get("frame_seq", 0) > 0:
                    return f
            time.sleep(0.75)
        return None

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
        # Its own timeout, and a generous one. `snapshot` re-renders the
        # scene and PNG-encodes it on the compositor's event loop, so the
        # session is blocked for every millisecond of it — an unoptimised
        # build on a machine that is busy compiling something else can sit
        # here for a minute. Timing out would report a contended box as a
        # broken compositor.
        ok(call(self.sock, {"op": "snapshot", "path": path},
                timeout=SNAPSHOT_TIMEOUT_S))
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
                 cut_dwell: float = 0.0, max_hold: float = 0.0) -> None:
        self.sessions = sessions
        self.by_name = {s.name: s for s in sessions}
        self.dir = directory
        self.interval = interval
        self.dwell_s = dwell
        self.cut_dwell_s = cut_dwell
        # The longest one unchanged frame is shown for. See `_timeline`.
        self.max_hold = max_hold
        # How much still picture the cap took out, filled in at compose.
        self.trimmed = 0.0
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

    def _timeline(self, t_start: float, t_end: float,
                  fps: int) -> list[tuple[str, str, float]]:
        """`(relative path, session, seconds on screen)`, in order.

        Each frame is shown from when it was taken until the next one
        was, so the video runs on the demo's own clock and is simply as
        dense as the camera managed. With one track there is nothing to
        pad against: no frame in the file is ever repeated, and none is
        interpolated.

        `max_hold` is the one edit. A session that is waiting — for a
        sandbox to spawn, for a package to unpack, for a change to cross
        a network — renders the same screen for as long as it waits, and
        filming that honestly still means minutes of a still picture.
        Capping how long one unchanged frame stays up trims that dead
        air. It cannot invent anything: every frame was really rendered
        by the session named on it, in this order, and a cap only ever
        shows one for *less* time than it was live. What it costs is that
        the video is no longer wall-clock; `trimmed` reports how much.
        """
        frames = self.frames
        if not frames:
            raise RuntimeError("no frames to compose")
        tick = 1.0 / fps
        cap = self.max_hold if self.max_hold > 0 else float("inf")
        out: list[tuple[str, str, float]] = []
        self.trimmed = 0.0
        for i, (t, rel, name) in enumerate(frames):
            begins = t_start if i == 0 else t
            # The last frame was the live state until the camera stopped;
            # it is shown for one tick and the video ends.
            ends = frames[i + 1][0] if i + 1 < len(frames) else t_end + tick
            lived = max(tick, ends - begins)
            shown = min(lived, max(tick, cap))
            self.trimmed += lived - shown
            out.append((rel, name, shown))
        return out

    def _concat_list(self, timeline: list[tuple[str, str, float]]) -> str:
        lines = ["ffconcat version 1.0"]
        for rel, _name, seconds in timeline:
            lines.append(f"file '{rel}'")
            lines.append(f"duration {seconds:.4f}")
        with open(os.path.join(self.dir, "track.txt"), "w") as f:
            f.write("\n".join(lines) + "\n")
        return "track.txt"

    def _label_spans(self, timeline: list[tuple[str, str, float]],
                     fps: int) -> list[tuple[str, float, float]]:
        """When each session's name is on screen.

        Walked over the same timeline the track is built from, so the
        label always names the session whose pixels are showing — not
        the one the camera had just been asked for, and not a moment
        that trimming has since moved.
        """
        spans: list[tuple[str, float, float]] = []
        at = 0.0
        for _rel, name, seconds in timeline:
            if spans and spans[-1][0] == name:
                spans[-1] = (name, spans[-1][1], at + seconds)
            else:
                spans.append((name, at, at + seconds))
            at += seconds
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
        timeline = self._timeline(t_start, t_end, fps)
        rel_track = self._concat_list(timeline)

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

        for i, (name, a, b) in enumerate(self._label_spans(timeline, fps)):
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


def wait_for(fn, timeout: float = SYNC_TIMEOUT_S, interval: float = 0.6):
    # Polled rather than pushed, and the interval is a real cost: every
    # probe here is a `state_dump`, which blocks the session it is asked
    # of — including, during a recording, the session being filmed. A
    # tighter loop buys no accuracy and takes frames off the camera.
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
# asking a running app what it holds
# --------------------------------------------------------------------


class DocumentReader:
    """A late joiner that asks one session's app for its document.

    Beat 7 is the only claim in this demo whose subject is a *client's*
    insides, and the compositor has no op that reads them: `state_dump`
    knows a foreign surface exists and painted, never what is written on
    it. Comparing two screenshots was the previous answer, and it is not
    an answer — a person has to look, and a run that films nothing
    reports the same silence as a run that films everything.

    So this asks the app instead, in the app's own protocol. It speaks
    the length-prefixed JSON `CollabRequest`/`CollabEvent` the toolkit
    speaks (`ui/src/collaboration.rs`), announces the same
    `(app_id, cluster_id, record)` address the word processor announces,
    and then sends the one frame every toolkit answers with its whole
    document: `Presence { Arrived }` (`collab_broker.rs:1149`, and the
    same handshake `qos-agent` opens with, `apps/agent/src/session.rs:72`).

    What comes back is therefore **produced by the app running in that
    session, out of the document that app is holding**. It is not the
    wire being read back to itself: a snapshot that carries the text
    means the far app's own document carries it, and a snapshot that
    does not means the two documents have not converged. Both are
    reported; neither is inferred from the other.
    """

    # The presence scope the word processor's toolkit answers on. Same
    # constant `qos-agent` uses, for the same reason: a different scope
    # is a different conversation and gets no reply.
    SCOPE = "word-processor"

    def __init__(self, sock_path: str, room: str, record: str | None,
                 handle: str = "readback") -> None:
        self.frames: list[dict] = []
        self.err: str | None = None
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(10.0)
        self.sock.connect(sock_path)
        self._send({"Hello": {"app_id": WORD_PROCESSOR, "cluster_id": room,
                              "record": record, "handle": handle}})
        self._send({"Presence": {"key": {"Workspace": {"id": self.SCOPE}},
                                 "payload": "Arrived"}})
        threading.Thread(target=self._read, daemon=True).start()

    def _send(self, obj: dict) -> None:
        body = json.dumps(obj).encode()
        self.sock.sendall(len(body).to_bytes(4, "little") + body)

    def _read(self) -> None:
        try:
            f = self.sock.makefile("rb")
            while True:
                hdr = f.read(4)
                if not hdr or len(hdr) < 4:
                    return
                body = f.read(int.from_bytes(hdr, "little"))
                if not body:
                    return
                try:
                    self.frames.append(json.loads(body.decode()))
                except Exception:
                    pass
        except Exception as e:              # the session went away
            self.err = str(e)

    def close(self) -> None:
        try:
            self.sock.close()
        except OSError:
            pass

    # -- what arrived ------------------------------------------------

    def documents(self) -> list[bytes]:
        """Every whole-document frame an app has answered with."""
        out = []
        for m in list(self.frames):
            if "CRDTUpdate" in m:
                out.append(bytes(m["CRDTUpdate"]["data"]))
        return out

    def deltas(self) -> list[bytes]:
        """Change frames, signed or not. Evidence of traffic, not of state."""
        out = []
        for m in list(self.frames):
            if "CRDTIncremental" in m:
                out.append(bytes(m["CRDTIncremental"]["data"]))
            elif "SignedCRDTIncremental" in m:
                env = m["SignedCRDTIncremental"].get("envelope") or {}
                payload = env.get("payload")
                if isinstance(payload, list):
                    out.append(bytes(payload))
        return out

    def await_document(self, needle: str, timeout: float):
        """Wait for a document frame, preferring one that carries `needle`.

        Returns `(document_bytes, carries_needle)`, or `(None, False)` if
        no app ever answered. Keeps waiting after the first snapshot
        arrives: a late joiner can be answered before the far app has
        applied the change being watched for, and the first frame is
        then a true document that is simply older than the question.
        """
        deadline = time.time() + timeout
        newest: bytes | None = None
        while time.time() < deadline:
            for doc in self.documents():
                newest = doc
                if _carries(doc, needle):
                    return doc, True
            for d in self.deltas():
                # A delta can carry the text too, and does when the far
                # app was already attached while it was typed.
                if _carries(d, needle):
                    return newest if newest is not None else d, True
            time.sleep(0.5)
        return newest, False


def _carries(blob: bytes, needle: str) -> bool:
    """Whether an Automerge blob contains `needle` as stored text.

    Automerge keeps a text object's characters as UTF-8 in a value
    column, so a phrase spliced into a document is present in the saved
    bytes — that is what makes this readable without an Automerge
    binding. Columns above a size threshold are DEFLATE'd, so any span
    that inflates is searched as well rather than being reported as an
    absence.
    """
    want = needle.encode()
    if want in blob:
        return True
    for start in range(len(blob)):
        if blob[start] != 0x78:      # zlib header, the only cheap anchor
            continue
        try:
            if want in zlib.decompressobj().decompress(blob[start:]):
                return True
        except zlib.error:
            continue
    return False


# --------------------------------------------------------------------
# the app stores each session sees
# --------------------------------------------------------------------
#
# "This machine does not have the word processor" is the whole of beat 5,
# so it has to be a fact about a real install root rather than a claim
# the script makes. Each session gets its own `registry.toml`, composed
# from the machine's: ada's has the word processor's entry removed,
# and the others keep it. Nothing is copied but the index — the entries
# carry absolute payload roots, so the payloads stay where they are.


def registry_entries(path: str) -> dict[str, str]:
    """Split a `registry.toml` into one verbatim text block per app.

    Text rather than parsed: an entry carries hashes and a publisher key,
    and round-tripping those through a hand-rolled TOML writer is a good
    way to install something that no longer verifies.
    """
    try:
        with open(path) as fh:
            body = fh.read()
    except OSError:
        return {}
    entries: dict[str, str] = {}
    for chunk in body.split("[[entry]]"):
        if not chunk.strip():
            continue
        ident = None
        for line in chunk.splitlines():
            if line.startswith("id = "):
                ident = line.split("=", 1)[1].strip().strip('"')
                break
        if ident:
            entries[ident] = "[[entry]]" + chunk.rstrip() + "\n"
    return entries


def compose_registry(apps_dir: str, entries: dict[str, str],
                     drop: tuple[str, ...] = ()) -> None:
    os.makedirs(apps_dir, exist_ok=True)
    kept = [v for k, v in sorted(entries.items()) if k not in drop]
    with open(os.path.join(apps_dir, "registry.toml"), "w") as fh:
        fh.write("\n".join(kept))


def installed_ids(apps_dir: str) -> list[str]:
    """What this session's install root actually holds, read off disk."""
    return sorted(registry_entries(os.path.join(apps_dir, "registry.toml")))


def a_real_png(width: int = 320, height: int = 200) -> bytes:
    """A picture with something in it.

    Written out rather than embedded so the image on screen is worth
    looking at: a 1x1 pixel proves the plumbing and shows the camera
    nothing.
    """
    import struct
    import zlib

    rows = bytearray()
    for y in range(height):
        rows.append(0)  # filter: none
        for x in range(width):
            rows += bytes((
                40 + (x * 160) // width,
                70 + (y * 150) // height,
                150 + ((x + y) * 90) // (width + height),
            ))

    def chunk(tag: bytes, payload: bytes) -> bytes:
        return (struct.pack(">I", len(payload)) + tag + payload
                + struct.pack(">I", zlib.crc32(tag + payload) & 0xFFFFFFFF))

    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(bytes(rows), 9))
            + chunk(b"IEND", b""))


def stage_home(path: str) -> str:
    """A home of this session's own, with the demo's folder in it."""
    demo = os.path.join(path, "Demo")
    os.makedirs(demo, exist_ok=True)
    with open(os.path.join(demo, "photo.png"), "wb") as fh:
        fh.write(a_real_png())
    with open(os.path.join(demo, "brief.md"), "w") as fh:
        fh.write("# Launch plan\n\n- Venue - The Hall\n- Date - 3 Sep\n")
    return path


class Registry:
    """A signed registry on loopback, serving one real `.qpkg`.

    The `serve-test-registry` example packs a real binary into a real
    package, signs it, and writes the `registries.toml` the catalogue
    reads — so the fetch in beat 5 verifies an ed25519 signature and a
    BLAKE3 digest for real. Absent binaries are a supported state: the
    beat says the fetch could not be filmed and the film goes on.
    """

    def __init__(self, example_bin: str, payload: str, config_dir: str,
                 log_path: str) -> None:
        self.example_bin = example_bin
        self.payload = payload
        self.config_dir = config_dir
        self.log_path = log_path
        self.proc: subprocess.Popen | None = None
        self._log = None
        self.base_url: str | None = None

    def __enter__(self) -> "Registry | None":
        if not (self.example_bin and os.path.exists(self.example_bin)
                and os.path.exists(self.payload)):
            return None
        os.makedirs(self.config_dir, exist_ok=True)
        self._log = open(self.log_path, "w+b")
        self.proc = subprocess.Popen(
            [self.example_bin, "--config-dir", self.config_dir,
             "--payload", self.payload, "--package-id", WORD_PROCESSOR,
             "--name", "Word Processor"],
            stdout=self._log, stderr=subprocess.STDOUT, start_new_session=True)
        # It announces its port once it is listening. Packing a large
        # binary happens before that line, so the wait is real work.
        deadline = time.time() + 180.0
        while time.time() < deadline:
            if self.proc.poll() is not None:
                print(f"  the test registry exited early "
                      f"({self.proc.returncode}); see {self.log_path}")
                return None
            with open(self.log_path, "rb") as fh:
                text = fh.read().decode(errors="replace")
            m = re.search(r"registry (http://\S+) serving", text)
            if m:
                self.base_url = m.group(1)
                return self
            time.sleep(0.3)
        print(f"  the test registry never announced a port; see {self.log_path}")
        return None

    def __exit__(self, *exc) -> bool:
        if self.proc is not None and self.proc.poll() is None:
            self.proc.send_signal(signal.SIGTERM)
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.proc.kill()
        if self._log is not None:
            self._log.close()
        return False


# --------------------------------------------------------------------
# the demonstration
# --------------------------------------------------------------------

# Set by `main`, so a beat can report on all three sessions without
# every beat function taking all three.
SESSIONS: list[Session] = []


def beat_one(ada: Session, shots: str) -> dict:
    banner("1.  an empty desktop, and Files opened on it")
    # The host's screen. A first boot really is empty here — one
    # workspace, no strips, no windows — so "empty" is read off the tree
    # rather than asserted about a backdrop.
    cut("ada")

    ids = {s.name: peer_id_of(s) for s in SESSIONS}
    for s in SESSIONS:
        note(f"{s.name:8} pid {s.proc.pid:<8} user {s.user:<8} peer {ids[s.name]}")
    claim("three separate processes", len({s.proc.pid for s in SESSIONS}), 3)
    claim("three separate libp2p identities", len(set(ids.values())), 3)

    host_ws = ada.active_workspace()
    note(f"ada's room {host_ws['room']}  named {host_ws['name']!r}")
    claim("ada owns his room", host_ws["owner"], ada.user)
    claim("his desktop starts with no windows on it",
          len(ada.tabs_here()), 0)
    ada.snapshot(os.path.join(shots, "01-empty-desktop.png"))

    step("he opens Files")
    # By its own id through the applications provider — the registry that
    # feeds the palette. There is no on-screen affordance for the first
    # window; the palette is a chord, and this is the same row it offers.
    painted = ada.launch(FILES_APP)
    claim("Files opened and painted a frame", painted is not None, True)
    if painted:
        note(f"surface {painted.get('surface_id')} at frame "
             f"{painted.get('frame_seq')}")
    tabs = ada.tabs_here()
    claim("and it is the only window on his desktop", len(tabs), 1)
    ada.snapshot(os.path.join(shots, "02-files.png"))
    return host_ws


def beat_two(ada: Session, bob: Session, host_ws: dict) -> None:
    banner("2.  ada invites bob")
    cut("ada")

    step("he mints an invite")
    # Minting is his: the palette opens on his screen and the card with
    # the token comes up there.
    token = mint_invite(ada)
    claim("ada minted an invite token", bool(token), True)
    if not token:
        raise SystemExit("no invite token: the rest cannot run")
    note("the token is a signed blob carrying the room id, the owner's "
         "address and an expiry")

    # Redeeming is bob's, and it is his screen that changes.
    cut("bob")
    step("bob redeems it")
    claim("bob joined", join_room(bob, token, by_action_api=False), True)
    claim("bob's own state holds ada's room",
          bob.active_workspace()["room"], host_ws["room"])
    claim("and his own copy names ada as owner",
          bob.active_workspace()["owner"], ada.user)


def beat_three(ada: Session, agent: Session, host_ws: dict) -> None:
    banner("3.  ada invites the agent")
    cut("ada")

    step("a second token — but an agent is not a peer")
    token = mint_invite(ada)
    claim("ada minted a second token", bool(token), True)
    if not token:
        raise SystemExit("no second token: the agent cannot join")

    cut("agent")
    claim("the agent joined the room", join_room(agent, token, by_action_api=True),
          True)
    claim("the agent's own state holds ada's room",
          agent.active_workspace()["room"], host_ws["room"])
    # The connect *prompt* was opened by address; the token itself had to
    # be typed, because the control socket has no semantic join op.
    gap("joining has no semantic op — the invite token had to be typed "
        "(only the prompt was opened by address)")

    step("what the agent is allowed to do, before ada says so")
    # The more interesting half, and the one that makes an agent a
    # different kind of participant: it is a subject in the permission
    # model, `agent:<id>`, and its authority is a grant on a scope. Asked
    # of the production gate, which answers about a verb on a scope.
    cut("ada")

    def may_write(app: str) -> bool:
        answer = ada.check_action(AGENT_SUBJECT, app, "SpliceText") \
            .get("answer", {}) or {}
        return bool(answer.get("permitted"))

    probe = ada.check_action(AGENT_SUBJECT, WORD_PROCESSOR, "SpliceText")
    note(f"check_action({AGENT_SUBJECT}, {WORD_PROCESSOR}, SpliceText) -> "
         f"{json.dumps(probe.get('answer', probe))[:140]}")
    claim("the agent may not write to the word processor yet",
          may_write(WORD_PROCESSOR), False)

    step(f"ada grants it write on {AGENT_SCOPE} — that app, and nothing else")
    granted = ada.grant(AGENT_SUBJECT, AGENT_SCOPE, "write")
    claim("the grant was accepted", "err" not in granted, True)
    if "err" in granted:
        note(json.dumps(granted["err"])[:200])
    claim("now it may — the same question, the same gate",
          bool(wait_for(lambda: may_write(WORD_PROCESSOR), timeout=SYNC_TIMEOUT_S)),
          True)
    # And the scope is a real boundary rather than a label: the grant
    # named one app, so it must not answer for another.
    claim("and still may not write to a different app",
          may_write("dev.qos.terminal"), False)
    note("a capability is a verb on a scope; the scope here is one app")

    step("and the session it drives is admitted to the room as itself")
    # The grant above is about the principal inside an app. The session
    # is a separate subject, and a guest who has only been let in is not
    # yet allowed to act — so it is admitted explicitly, the same way
    # bob would be.
    admitted = ada.grant(f"person:{agent.user}", "workspace", "write")
    claim("ada admits the agent's session as an editor",
          "err" not in admitted, True)
    if "err" in admitted:
        note(json.dumps(admitted["err"])[:200])


def beat_four(ada: Session, bob: Session, agent: Session,
              host_ws: dict, shots: str) -> None:
    banner("4.  both have joined — and each session can see the others")

    step("read ada's room back out of the other two")
    # Neither of these is ada's own state. Each guest independently
    # reports the room it now holds, so the camera sits on the guest
    # doing the reporting.
    cut("bob")
    claim("bob's own copy carries ada's name for the room",
          bob.active_workspace()["name"], host_ws["name"])
    cut("agent")
    claim("the agent's own copy carries it too",
          agent.active_workspace()["name"], host_ws["name"])

    step("and the cursors")
    # `peers` is read from the same PresenceCtx the overlay draws from,
    # which is the only way to tell "no peer arrived" from "a peer
    # arrived and was not drawn" — those make the same picture.
    cut("ada")
    ada.pointer_move(880.0, 520.0)
    ada.settle(0.6)
    cut("bob")
    bob.pointer_move(640.0, 610.0)
    bob.settle(0.6)

    def seen_by(watcher: Session, who: str):
        rows = (watcher.peers() or {}).get("pointers") or []
        return [r for r in rows if r.get("user") == who] or None

    on_bob = wait_for(lambda: seen_by(bob, ada.user), timeout=SYNC_TIMEOUT_S)
    if on_bob:
        note(f"bob's screen holds {json.dumps(on_bob[0])}")
        claim("bob's session paints ada's cursor, under his user id",
              bool(on_bob), True)
    else:
        gap("no cursor is painted for a peer at the desktop level, and "
            "none is expected to be: `RemoteCursorOverlay` "
            "(shell/src/shell/remote_cursors.rs) is mounted in tests and "
            "nowhere in the native compositor. Presence is a property of a "
            "shared *app*, not of the strip — so the cursors this "
            "storyboard promises are filmed in beat 7, inside the document "
            "both of them have open, and this beat shows only that the "
            "room itself crossed")
    bob.snapshot(os.path.join(shots, "03-bob-joined.png"))

    cut("ada")
    on_ada = wait_for(lambda: seen_by(ada, bob.user), timeout=10.0)
    if on_ada:
        claim("and ada's paints bob's, the other way",
              bool(on_ada), True)
    else:
        gap("the same in the other direction, for the same reason — a "
            "desktop-level cursor overlay is not part of the native "
            "compositor")


def beat_five(ada: Session, registry, shots: str) -> dict | None:
    banner("5.  \"word processor\" — a machine that does not have it")
    cut("ada")

    installed = set(installed_ids(ada.apps_dir))
    claim("this machine does not have the word processor",
          WORD_PROCESSOR in installed, False)
    note(f"{len(installed)} applications installed; {WORD_PROCESSOR} is not "
         f"one of them")
    if registry is None:
        gap("no local registry was served, so the fetch could not be filmed: "
            "build it with `cargo build -p app-catalogue --bins --examples` "
            "and `cargo build -p qos-install --bin qos-install`")
        return None
    note(f"a registry at {registry.base_url} offers it")

    step("he searches for it, and the row is offered like any other")
    ada.modifiers(logo=True)
    ada.key("k")
    ada.modifiers()
    ada.settle(0.4)
    ada.type_text("word processor")
    ada.settle(1.2)
    pal = ada.palette()
    rows = pal.get("rows") or []
    offered = next((r for r in rows if WORD_PROCESSOR in json.dumps(r)), None)
    claim("the palette offers an app this machine does not have",
          offered is not None, True)
    if offered:
        note(f"row: {offered.get('title')!r} — {offered.get('subtitle')!r}")
    ada.snapshot(os.path.join(shots, "04-palette-offer.png"))

    step("he presses Enter — fetch, verify, unpack, run")
    before = {f.get("surface_id") for f in ada.foreign_frames()}
    ada.key("Enter")
    began = time.time()
    painted = None
    deadline = time.time() + LAUNCH_TIMEOUT_S * 2
    while time.time() < deadline:
        for f in ada.foreign_frames():
            if f.get("surface_id") not in before and f.get("frame_seq", 0) > 0:
                painted = f
                break
        if painted:
            break
        time.sleep(0.75)
    took = time.time() - began
    claim("the word processor was fetched and opened", painted is not None, True)
    if painted is None:
        note(f"see {ada.log_path}")
        return None
    note(f"{took:.1f}s from Enter to first frame — one machine, one LAN hop, "
         f"an unoptimised payload; not a number to quote")
    claim("and it is installed now, on the machine that lacked it",
          WORD_PROCESSOR in set(installed_ids(ada.apps_dir)), True)
    ada.snapshot(os.path.join(shots, "05-word-processor.png"))
    return painted


def beat_six(ada: Session, bob: Session, shots: str) -> str | None:
    banner("6.  cut to bob — the window is open for him too")

    # Where bob is looking, before anything arrives. Both halves of the
    # rule get asserted, because either alone is easy to satisfy wrongly:
    # the window must arrive, *and* he must not be moved to it.
    cut("bob")
    before_view = bob.view_state()
    before_groups = bob.groups_here()
    q_tabs = set(ada.tabs_here())
    fresh = [t for t in q_tabs if t not in set(bob.tabs_here())]
    note(f"ada holds {len(q_tabs)} tab(s); bob holds "
         f"{len(bob.tabs_here())}")

    step("the tab ada's word processor opened, read out of bob's tree")
    if not fresh:
        gap("ada's session had no tab bob lacked, so there was "
            "nothing to watch cross")
        return None
    tid = sorted(fresh)[0]
    landed = wait_for(lambda: tid in bob.tabs_here(), timeout=DOC_SYNC_TIMEOUT_S)
    claim("the window ada opened is open for bob too", bool(landed), True)
    if landed:
        arrived = bob.tabs_here()[tid]
        note(f"tab {tid} in bob's workspace, titled {arrived.get('title')!r}")
    else:
        note(f"tab {tid} had not replicated after {DOC_SYNC_TIMEOUT_S:.0f}s")

    step("and his own view did not move")
    after_view = bob.view_state()
    claim("bob's active workspace did not move",
          after_view["active"], before_view["active"])
    claim("bob's scrolled-to strip did not move",
          after_view["active_strip"], before_view["active_strip"])
    claim("bob's focus did not move",
          after_view["active_group"], before_view["active_group"])
    for g, was in before_groups.items():
        if g in bob.groups_here():
            claim(f"the tab in front of him in group {g[:6]}… did not change",
                  bob.groups_here()[g]["active_tab"], was["active_tab"])
    note("which windows exist is the workspace; which one you are looking "
         "at is you, and the second does not travel")
    bob.snapshot(os.path.join(shots, "06-bob-window.png"))
    return tid


def beat_seven(ada: Session, bob: Session, host_ws: dict,
               shots: str) -> None:
    banner("7.  ada writes, and bob's screen is asked what it holds")
    room = host_ws["room"]

    step("bob opens the word processor on his own machine")
    cut("bob")
    # His machine runs its own copy and draws it locally. Nothing is
    # streamed; the two copies share one document.
    his = bob.launch(WORD_PROCESSOR)
    claim("bob's word processor painted", his is not None, True)
    if his is None:
        gap("bob has no word processor open, so there is no far screen "
            "to read a document back off")
        return
    # Both apps have to have attached to their broker and announced their
    # address before either types, or the first keystrokes go out to
    # nobody and it reads as a transport failure.
    bob.settle(6.0)

    step(f"ada types {TYPED_TEXT!r}")
    cut("ada")
    # Into the document body: a launch leaves focus wherever the chrome
    # had it, and text typed at the chrome is text the document never sees.
    ada.click_at(960.0, 620.0)
    ada.settle(1.5)
    ada.type_text(TYPED_TEXT)
    ada.settle(1.0)
    ada.snapshot(os.path.join(shots, "07-ada-typed.png"))

    step("and bob's session is asked what it now holds")
    cut("bob")
    # What is mechanically readable from here is presence: the caret and
    # pointer of the peer who is typing, attributed to his user id, off
    # the same context the overlay paints from.
    def ada_present():
        rows = (bob.peers() or {}).get("pointers") or []
        return [r for r in rows if r.get("user") == ada.user] or None

    seen = wait_for(ada_present, timeout=SYNC_TIMEOUT_S)
    if seen:
        claim("bob's screen carries ada while he is typing",
              bool(seen), True)
    else:
        gap("ada's presence did not reach bob's session")

    step("and his word processor is asked for the document it is holding")
    # The beat itself, and the one claim whose subject is inside a
    # client. Asked of bob's *app*, in the app's own protocol: a late
    # joiner on his session's broker announces the word processor's
    # address and sends the `Arrived` every toolkit answers with its
    # whole document. What comes back was produced by his running app
    # out of the document it holds, so the answer is about his screen
    # and not about the wire.
    #
    # The camera stays on bob for the whole wait, so if the document
    # converges the arrival is on film rather than described afterwards.
    reader = None
    doc, carried = None, False
    try:
        reader = DocumentReader(bob.collab_json_sock, room, record=None)
        doc, carried = reader.await_document(TYPED_TEXT, DOC_SYNC_TIMEOUT_S)
    except OSError as e:
        gap(f"bob's session has no JSON broker to ask ({e}) — "
            f"looked at {bob.collab_json_sock}")
    finally:
        if reader is not None:
            reader.close()

    if carried:
        claim("bob's own word processor holds what ada typed",
              carried, True)
        note(f"answered with a {len(doc)} byte document carrying "
             f"{TYPED_TEXT!r} — produced by his app, not by the wire")
    elif doc is not None:
        # A document really did come back, and really does not have it.
        # This is the honest failure: the two apps are attached, they
        # answer each other's handshake, and their documents are still
        # two documents.
        gap(f"bob's word processor answered with a {len(doc)} byte "
            f"document that does not carry {TYPED_TEXT!r} — the two "
            f"documents have NOT converged. Presence crosses and the "
            f"document does not; nothing on his screen shows what "
            f"ada wrote")
    else:
        gap(f"no app in bob's session answered the late-join handshake "
            f"within {DOC_SYNC_TIMEOUT_S:.0f}s, so his document could not "
            f"be read back at all — the beat did NOT converge, and this "
            f"run cannot say whether the cause is the document path or "
            f"the handshake. See {bob.log_path}")
    bob.settle(2.0)
    bob.snapshot(os.path.join(shots, "08-bob-received.png"))


def beat_eight(ada: Session, shots: str) -> None:
    banner("8.  an image into the document, through the OS file chooser")
    cut("ada")

    # Which of the two routes this films, decided on the code rather
    # than on preference. The built drag is promoted from the
    # compositor's *own* file-manager chrome; the Files on screen here is
    # the launched `dev.qos.file-explorer`, a sandboxed Wayland client,
    # and neither it nor the toolkit ever opens a `wl_data_source`
    # (`grep -rn 'start_drag\|wl_data_source' apps/file-explorer/src
    # ui/src` finds only in-process tab dragging). There is no client
    # drag for the compositor to promote, so there is nothing to film.
    #
    # The chooser is the better beat anyway, and not as a consolation:
    # the drag would show a file moving, and this shows the sandbox —
    # the app asks, the OS chooses, and what the app is handed is bytes
    # rather than a path it has no right to read.
    note("filming the OS file chooser, not a drag: the launched Files is a "
         "sandboxed client and opens no wl_data_source, so no client drag "
         "exists for the compositor to promote")
    note("the app cannot read the folder; the OS hands it the bytes")
    photo = os.path.join(ada.home, "Demo", "photo.png")
    claim("there is an image to insert", os.path.exists(photo), True)

    step("the word processor asks the OS for a file")
    before = set(ada.target_names())
    # The toolbar belongs to the client, and a client's insides are not in
    # the compositor's widget registry, so this is the one gesture in the
    # demo aimed at a coordinate rather than at a name.
    tile = next((n for n in before if n.startswith("foreign-surface-")), None)
    if tile is None:
        gap("no client tile is laid out, so the toolbar cannot be reached")
        return
    rect = ada.named_rect(tile)
    if not rect:
        gap(f"{tile} has no laid-out rect")
        return
    tx, ty, tw, th = rect
    note(f"tile {tile} at {[round(v) for v in rect]}")
    opened = None
    # The image control sits in the toolbar strip under the tile's top
    # edge. Swept rather than guessed once: a miss is a click on inert
    # chrome, and the chooser opening is what says it was hit.
    for frac in (0.34, 0.40, 0.46, 0.28, 0.52, 0.58, 0.22, 0.64):
        ada.click_at(tx + tw * frac, ty + th * 0.055)
        ada.settle(1.1)
        names = set(ada.target_names())
        fresh = {n for n in names if n not in before}
        if any("chooser" in n.lower() or "file_row" in n.lower()
               or "portal" in n.lower() for n in fresh):
            opened = sorted(fresh)[:8]
            break
    if opened is None:
        gap("the word processor's image control was not found by sweeping "
            "its toolbar — a client's widgets are not addressable by name "
            "from the compositor, so this beat is aimed at pixels and did "
            "not land")
        ada.snapshot(os.path.join(shots, "09-no-chooser.png"))
        return
    note(f"the chooser is compositor chrome: {opened}")
    claim("the OS file chooser opened, not the app's own", True, True)
    ada.snapshot(os.path.join(shots, "09-chooser.png"))

    step("he picks photo.png")
    row = next((n for n in ada.target_names()
                if "photo.png" in n), None)
    if row is None:
        gap("photo.png was not offered by the chooser")
        return
    ada.click_named(row)
    ada.settle(1.5)
    ada.key("Enter")
    ada.settle(3.0)
    ada.snapshot(os.path.join(shots, "10-image.png"))
    note("the image is bytes inside the document, so it replicates the "
         "way the text does")


def beat_nine(ada: Session, bob: Session, agent: Session,
              shots: str) -> None:
    banner("9.  the agent acts, by address")
    cut("agent")

    baseline = agent.input_ops
    note(f"synthetic-input commands sent to the agent so far: {baseline}")

    step("it enumerates what this session exports")
    surface = agent.enumerate_actions("")
    actions = surface["actions"]
    providers: dict[str, int] = {}
    for a in actions:
        providers[a["provider"]] = providers.get(a["provider"], 0) + 1
    note(f"{surface['count']} addressable actions across "
         f"{len(providers)} providers")
    claim("every entry carries provider, id, kind and title",
          all(isinstance(a.get(k), str) and a[k]
              for a in actions for k in ("provider", "id", "kind", "title")), True)
    claim("no entry carries a screen position",
          any(k in a for a in actions for k in ("rect", "x", "y")), False)

    step("enumerating is read-only: surveying cannot fire a side-effect")
    before_chrome = agent.chrome()
    for query in ("", "room", "word"):
        agent.enumerate_actions(query)
    after_chrome = agent.chrome()
    claim("the chrome revision did not move",
          after_chrome["revision"], before_chrome["revision"])
    claim("the palette did not open", after_chrome["palette"]["visible"], False)

    step("it invokes one action by its address")
    picked = next((a for a in actions if a["provider"] == AGENT_PROVIDER
                   and a["id"] == AGENT_ACTION_ID), None)
    claim(f"the catalogue offers ({AGENT_PROVIDER}, {AGENT_ACTION_ID})",
          picked is not None, True)
    if picked is None:
        raise SystemExit("the agent's chosen action is not exported")
    claim("the effect is absent beforehand",
          agent.overlays()[AGENT_ACTION_EFFECT], False)
    resp = agent.invoke_action(AGENT_PROVIDER, AGENT_ACTION_ID)
    claim("the invocation was accepted", "answer" in resp, True)
    claim("the reply names the address that ran",
          (resp.get("answer", {}).get("provider"), resp.get("answer", {}).get("id")),
          (AGENT_PROVIDER, AGENT_ACTION_ID))
    landed = wait_for(lambda: agent.overlays()[AGENT_ACTION_EFFECT], timeout=8)
    claim("the action changed live state", bool(landed), True)
    agent.snapshot(os.path.join(shots, "11-agent.png"))

    step("no synthetic input was used to do it")
    claim("no keystroke, click or modifier reached the agent since",
          agent.input_ops, baseline)
    note("the address (provider, id) is the entire input to the invocation")

    step("and it opens a window, which the other two receive")
    # The claim worth filming: the agent is a participant in the room, so
    # what it does lands in everyone's copy of it. Read back from the two
    # sessions that did not make the change.
    before_agent = set(agent.tabs_here())
    opened = agent.invoke_action(AGENT_PROVIDER, "window.new.terminal")
    if "err" in opened:
        gap(f"the agent may not open a window here — "
            f"{json.dumps(opened['err'])[:120]}")
        return
    fresh = wait_for(lambda: (set(agent.tabs_here()) - before_agent) or None,
                     timeout=SYNC_TIMEOUT_S)
    claim("the agent's own session shows the window", bool(fresh), True)
    if not fresh:
        return
    tid = sorted(fresh)[0]
    claim("no synthetic input was used for that either",
          agent.input_ops, baseline)

    cut("ada")
    on_q = wait_for(lambda: tid in ada.tabs_here(), timeout=DOC_SYNC_TIMEOUT_S)
    claim("the window the agent opened is open for ada", bool(on_q), True)
    cut("bob")
    on_x = wait_for(lambda: tid in bob.tabs_here(), timeout=DOC_SYNC_TIMEOUT_S)
    claim("and for bob", bool(on_x), True)
    if not (on_q and on_x):
        note(f"tab {tid} had not replicated after {DOC_SYNC_TIMEOUT_S:.0f}s")
    bob.snapshot(os.path.join(shots, "12-bob-agent.png"))


def run(ada: Session, bob: Session, agent: Session, shots: str,
        registry) -> None:
    host_ws = beat_one(ada, shots)
    beat_two(ada, bob, host_ws)
    beat_three(ada, agent, host_ws)
    beat_four(ada, bob, agent, host_ws, shots)
    beat_five(ada, registry, shots)
    beat_six(ada, bob, shots)
    beat_seven(ada, bob, host_ws, shots)
    beat_eight(ada, shots)
    beat_nine(ada, bob, agent, shots)




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
    ap.add_argument("--record-dwell", type=float, default=1.1, metavar="S",
                    help="seconds to hold on each beat and step while "
                         "recording, so the video runs long enough to read. "
                         "Real elapsed time, really sampled — never a held "
                         "frame standing in for one")
    ap.add_argument("--record-cut-dwell", type=float, default=1.5, metavar="S",
                    help="seconds to hold after cutting to a session, so the "
                         "screen you have just been moved to registers "
                         "before the demo carries on")
    ap.add_argument("--record-max-hold", type=float, default=1.8, metavar="S",
                    help="longest one unchanged frame is shown for. A "
                         "session waiting on a sandbox spawn or a network "
                         "hop renders the same screen for as long as it "
                         "waits; this trims that dead air. Nothing is "
                         "duplicated or interpolated — a frame is only ever "
                         "shown for less time than it was live. Keep it "
                         "above --record-dwell, or the holds the demo asks "
                         "for are trimmed along with the accidental ones. "
                         "0 keeps the video on wall-clock time")
    ap.add_argument("--record-fps", type=int, default=30, metavar="N",
                    help="container frame rate of the composed video; the "
                         "capture rate is whatever was achieved, and frames "
                         "are held, never interpolated, to fill it")
    args = ap.parse_args()

    # The spawn site reads this: only a run that means to leave sessions
    # behind detaches them from this process's fate.
    global KEEP_SESSIONS
    KEEP_SESSIONS = args.keep

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

    # The packaging chain, by path. Each is optional: a missing one costs
    # the beat that needs it and nothing else, and the transcript says
    # which. `demo` binaries first, because that is what the compositor
    # is built as, falling back to `debug` where only one exists.
    def tool(name: str) -> str | None:
        for profile in ("demo", "debug"):
            path = os.path.join(REPO, "target", profile, name)
            if os.path.exists(path):
                return path
        return None

    tools = {var: path for var, name in (
        ("QOS_CATALOGUE_BIN", "qos-catalogue"),
        ("QOS_INSTALL_BIN", "qos-install"),
        ("QOS_LAUNCHER_BIN", "qos-launcher"),
    ) if (path := tool(name))}

    # Every session's view of what is installed, composed from the
    # machine's index. The word processor's entry is taken from the
    # registry built out of *this* tree where there is one: the machine's
    # payload is an older static binary, and a run against it would be
    # filming a build that predates everything under test.
    entries = registry_entries(os.path.join(MACHINE_APPS, "registry.toml"))
    live = registry_entries(os.path.join(LIVE_APPS, "registry.toml"))
    if WORD_PROCESSOR in live:
        entries[WORD_PROCESSOR] = live[WORD_PROCESSOR]
        print(f"word processor: {LIVE_APPS} (built from this tree)")
    elif WORD_PROCESSOR in entries:
        print(f"word processor: {MACHINE_APPS} — no registry at {LIVE_APPS}, "
              f"so this films the machine's installed payload")

    sessions = []
    for short, name in zip("qxa", ("ada", "bob", "agent")):
        run_dir = os.path.join(args.out, name)
        apps_dir = os.path.join(run_dir, "qos-apps")
        # ada is the machine that does not have it yet. Everyone
        # else does, so bob can open his own copy in beat 7.
        compose_registry(apps_dir, entries,
                         drop=(WORD_PROCESSOR,) if name == "ada" else ())
        sessions.append(Session(
            name, name, run_dir,
            os.path.join(runtime, f"qos-yc-{short}-{pid}.sock"),
            args.shell, size,
            # A private runtime dir per session, standing in for the
            # per-user one logind would give each real login.
            os.path.join(runtime, f"qyc-{short}{pid}"),
            stage_home(os.path.join(run_dir, "home")),
            apps_dir, tools))
    ada, bob, agent = sessions
    global SESSIONS
    SESSIONS = sessions

    print(f"compositor:    {args.shell}")
    print(f"run directory: {args.out}")
    print(f"apps:          {len(entries)} installed for bob and the agent, "
          f"{len(entries) - (1 if WORD_PROCESSOR in entries else 0)} for ada")
    for var, path in sorted(tools.items()):
        print(f"  {var:20} {path}")
    for var in ("QOS_CATALOGUE_BIN", "QOS_INSTALL_BIN", "QOS_LAUNCHER_BIN"):
        if var not in tools:
            print(f"  {var:20} MISSING — beat 5 cannot fetch anything")

    global RECORDER
    record_dir = None
    if args.record is not None:
        record_dir = args.record or os.path.join(args.out, "recording")
        record_dir = os.path.abspath(record_dir)
        os.makedirs(record_dir, exist_ok=True)
        RECORDER = Recorder(sessions, record_dir, args.record_interval,
                            args.record_dwell, args.record_cut_dwell,
                            args.record_max_hold)
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
        # The registry ada fetches from in beat 5. Its config dir is
        # his store's, which is how the catalogue in his session finds it.
        with Registry(
            os.path.join(REPO, "target", "debug", "examples",
                         "serve-test-registry"),
            os.path.join(LIVE_APPS, "payloads", WORD_PROCESSOR, "bin",
                         "word-processor"),
            os.path.join(ada.dir, "store-config"),
            os.path.join(args.out, "registry.log"),
        ) as registry:
            if registry is None:
                print("no local registry: beat 5 will report the fetch as "
                      "unfilmed rather than skip past it")
            else:
                print(f"registry:      {registry.base_url}")
            run(ada, bob, agent, shots, registry)
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
        print(f"             every frame is a real render, in capture order; "
              f"none repeated, none interpolated")
        if RECORDER.trimmed > 0.5:
            span = RECORDER.stopped_at - RECORDER.started_at
            print(f"             {RECORDER.trimmed:.0f}s of still picture "
                  f"trimmed from {span:.0f}s of demo — waits on sandbox "
                  f"spawns and network hops, held at most "
                  f"{args.record_max_hold:.1f}s each")
    return rc


if __name__ == "__main__":
    sys.exit(main())
