#!/usr/bin/env python3
"""Run three Tilo sessions in one room, with an agent driving one of them.

Starts three isolated compositors with separate state directories, Iroh
endpoint keys, user identities and control sockets:

  ada  hosts the room and owns it
  bob    joins it by invite
  Claude   is admitted to it too, and from then on is driven *only* by
           the action API: semantic invocation by `(provider, id)`,
           never by a click, a keystroke, or a pixel

The storyboard, in order:

  1. an empty desktop, and Files opened on it
  2. ada invites bob
  3. ada invites the agent — a capability principal, not a peer
  4. both have joined, and each session can see the others
  5. "word processor" — a machine that does not have it, fetching it
  6. cut to bob: the window ada opened is open for him too
  7. ada writes, and bob's screen is asked what it holds
  8. a photograph dragged out of Files and into the document
  9. the agent acts by address, and the other two read the result back

Each claim is read back from a session that did **not** make the change.
When that is not possible, the transcript prints a GAP.

    scripts/demo/run.py
    scripts/demo/run.py --keep
    scripts/demo/run.py --record            # …and write a 1920x1080 video

Runs against target/demo/shell:

    cargo build --profile demo -p shell -p tilo-control --bin shell --bin tilo-control

Build it optimised. `snapshot` re-renders the scene and PNG-encodes it
on the compositor's event loop; an unoptimised build spends ~1.4 s per
1920x1080 frame.

`--release` omits the `TILO_TEST_CONTROL` socket because it is a complete
lock-screen bypass. `demo` is the release profile with that test gate open.
"""
from __future__ import annotations

import ast
import argparse
import contextlib
import hashlib
import base64
import json
import os
import random
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
    0, os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "headless-examples"))
from reaper import die_with_parent, track   # noqa: E402

# Set from `--keep`. Off means the kernel reaps a compositor with this
# script, however this script dies.
KEEP_SESSIONS = False

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))


def _shadowed_definitions() -> list[str]:
    """Top-level names this file defines more than once.

    Python resolves a redefinition silently, in favour of whichever came
    last, so a duplicate name is not an error anywhere — it is a
    different function than the one at the call site, called with the
    wrong kind of argument, failing inside a `try` that turns it into a
    plausible-looking answer. That is exactly how a second
    `peer_id_of` — one taking a Session, one taking an invite token —
    reported that three separate compositors shared one identity, and
    sent people looking for a regression in the shell.

    A harness whose whole purpose is to tell truth from plausible
    failure should not be able to fail that way quietly.
    """
    try:
        tree = ast.parse(open(os.path.abspath(__file__)).read())
    except (OSError, SyntaxError):
        return []
    seen: dict[str, int] = {}
    dupes: list[str] = []
    for node in tree.body:
        if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef,
                                 ast.ClassDef)):
            continue
        if node.name in seen:
            dupes.append(f"{node.name} (lines {seen[node.name]} and "
                         f"{node.lineno})")
        seen[node.name] = node.lineno
    return dupes


def _doc_text_bin() -> str | None:
    """The Automerge decoder a document assertion needs.

    A saved document is DEFLATE-compressed per column, so there is no
    honest way to answer "does this hold that sentence" without decoding
    it. Absent, the beats that ask are reported as gaps rather than
    quietly answered wrong.
    """
    for profile in ("demo", "debug"):
        path = os.path.join(REPO, "target", profile, "tilo-doc-text")
        if os.path.exists(path):
            return path
    return None


DOC_TEXT = _doc_text_bin()

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
FILES_APP = "so.tilo.file-explorer"
WORD_PROCESSOR = "so.tilo.word-processor"

# Machine state, deliberately *not* isolated per session: without these
# two a session enumerates zero applications and every launch fails with
# "no bundle was found". Documents and identities stay per-session; the
# installed-app store and the runtime bundle are the machine's.
# Where the three sessions' home directories live.
#
# Deliberately *not* under `--out`. The run directory is inside the
# checkout, and Files draws the path it is showing: a home under
# `target/` puts the worktree, `.claude` and the branch name on screen
# in the first shot of the film. Every component of this one reads as a
# machine's own, and the trail ends `… › home › ada`.
#
# It is still throwaway state — wiped and rebuilt per run, like the run
# directory — just somewhere that does not name the tooling.
#
# It cannot be `/home/<user>`, which is what would render perfectly:
# that is the real user's home and is not ours to write into. See the
# note in `stage_home`; the demo must not expose the staging path.
HOMES_ROOT = "/var/tmp/tilo/home"

# The photograph beat 8 drags into the document, by the name it wears on
# screen. Named once because four places have to agree about it: the
# staging that writes it, the assertion that it is there, the row the
# press has to land on, and the chip the compositor paints at the cursor
# while it crosses — that last one reads the name out of the `text/uri-list`
# the source app is offering, so what is on camera is the file's real name
# rather than anything this script told it to say.
PHOTO_NAME = "harbour.png"

MACHINE_APPS = os.path.expanduser("~/.local/share/tilo-apps")
RUNTIME_BUNDLES = os.path.expanduser("~/.local/share/tilo-runtime")
# A registry holding the word processor built from *this* tree. The
# machine's installed payload is a static binary from an earlier build
# and cannot exercise anything that has changed in `ui` since, so a run
# against it would be filming last month's app.
LIVE_APPS = os.path.join(REPO, "target", "tilo-apps-live")

# The capability principal ada grants in beat 3. An agent is not a
# peer: it is a subject in the permission model, and the grant is scoped
# to one app rather than to the workspace.
AGENT_SUBJECT = "agent:claude"
AGENT_SCOPE = f"app:{WORD_PROCESSOR}"

# How fast a person types, on camera.
#
# Not a speed setting — before this, runs of unshifted characters went
# out in a single `type` call, so a sentence arrived in two or three
# instant bursts. The viewer saw text appear without seeing it typed,
# which is the one thing the beat is for.
#
# 55 ms is around 60 words a minute: quick, clearly deliberate, and slow
# enough that the caret is visibly ahead of the words.
TYPING_PACE = 0.055

# Where hands actually slow down. A person does not type at one rate —
# they pause fractionally after a word and longer after a sentence.
TYPING_PAUSE = {
    " ": 0.085,
    ",": 0.16,
    ".": 0.22,
    ":": 0.2,
    "\n": 0.3,
}

# And no two keystrokes take the same time. An exactly even cadence is
# the tell — it is the one thing real typing never does, and at 18 keys
# a second the eye picks it out as a machine even when the speed is
# right. Each interval is scaled by a factor drawn around 1.0, so the
# rhythm wanders the way a hand does.
#
# Seeded, not arbitrary: two runs of the film type identically, which is
# what lets a frame from one run be compared against a frame from
# another. The spread is deliberately asymmetric — a hand has a floor it
# cannot go below and a long tail of hesitations above it, so the
# distribution is skewed rather than centred.
TYPING_JITTER = (0.72, 1.45)
TYPING_SEED = 0x9E3779B9


def _typing_jitter(who: str):
    """An endless run of interval multipliers for `who`'s hands.

    Its own `random.Random`, seeded from the name, rather than the module
    generator: two sessions type at once in this film, and drawing from a
    shared generator would make each person's rhythm depend on how their
    keystrokes happened to interleave with the other's — so a rerun that
    scheduled them differently would type differently.

    `triangular` rather than `uniform` because a hand is not uniform: it
    has a floor it cannot beat and a tail of hesitations above it. The
    mode sits below the midpoint, so most keys land near the base pace
    and the occasional one drags.
    """
    lo, hi = TYPING_JITTER
    rng = random.Random(TYPING_SEED ^ hash(who) & 0xFFFF_FFFF)
    while True:
        yield rng.triangular(lo, hi, lo + (hi - lo) * 0.35)

# What ada types into the document, and what bob's session is then
# asked whether it holds.
TYPED_TEXT = "the same document on both machines"

# Characters a US layout only produces with shift held, and the key that
# carries each one. `Session.type_prose` presses a real Shift across
# these; without it a client resolves the bare keycode and puts the
# character *below* the one that was asked for on screen.
SHIFTED_KEYS = {c: c.lower() for c in "ABCDEFGHIJKLMNOPQRSTUVWXYZ"}
SHIFTED_KEYS.update({
    "!": "1", "@": "2", "#": "3", "$": "4", "%": "5", "^": "6",
    "&": "7", "*": "8", "(": "9", ")": "0", "_": "-", "+": "=",
    "{": "[", "}": "]", "|": "\\", ":": ";", '"': "'",
    "<": ",", ">": ".", "?": "/", "~": "`",
})

# Beat 9. The agent reads its instruction out of the document itself —
# a paragraph addressed to it by name (`apps/agent/src/plan.rs`) — and
# turns the run of bullets immediately above into a table. So the list
# has to be there first, and it has to be worth reading: the failure
# this replaces produced a table with nothing in it, which a transcript
# asking only "is there a table" cannot tell from success.
# `- ` at the start of a paragraph autoformats into a list item
# (`word_processor.rs:2160`), and Enter continues the list, so one
# prefix makes the whole run. It has to *be* a run of `list_item`
# blocks: the agent converts those and nothing else
# (`plan.rs::bullets_above`), and a run of plain paragraphs is what
# produced a table with empty cells. The `: ` splits each line into two
# columns (`plan.rs::split_cells`).
AGENT_BULLETS = "\n- Doors: 18:00\nSound check: 18:30\nSeating: 240\n\n"
AGENT_INSTRUCTION = "Claude: turn the bullets into a table\n"
# One cell's text, looked for in the document afterwards. If the table
# came out empty this is gone, and the claim fails instead of passing on
# the existence of a grid.
AGENT_BULLET_PROOF = "Sound check"

# How long to wait for a change to cross between sessions.
SYNC_TIMEOUT_S = 25.0
# A document crossing is a longer chain than a room-index entry: publish,
# gossip, drain on the far side, apply, paint. Given its own budget so a
# slow hop is not reported as a failure to converge.
DOC_SYNC_TIMEOUT_S = 60.0
# An app launch is a sandbox spawn, a Wayland connection, a toolkit
# starting up and a first frame. Generous because none of that is Tilo
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

    #: Whether this session gossips its presence to peers on the network.
    #: On for the nine-beat film, whose whole subject is other people;
    #: worth turning off for a single-session take, because an explicit
    #: demo peer or configured host would paint a cursor and name pill over
    #: the document on camera.
    peer_sync: bool = True

    def __init__(self, name: str, user: str, run_dir: str, sock: str, binary: str,
                 size: str, runtime_dir: str, home: str, apps_dir: str,
                 tools: dict[str, str] | None = None):
        self.name = name
        self.user = user
        # This session's own typing rhythm, seeded from its name so each
        # person types a little differently and each of them types the
        # same way every run.
        self._typing_jitter = _typing_jitter(name)
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
        # binds its collab broker at <runtime>/tilo/services/collab.sock,
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
        return os.path.join(self.runtime_dir, "tilo", "services", "collab.sock")

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
        return os.path.join(self.runtime_dir, "tilo", "services",
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
        # One language on screen. An app reads `LC_ALL`, then `LANGUAGE`,
        # then `LANG` (`apps/word-processor/src/i18n.rs:88`), bwrap does
        # not clear the environment, and the machine filming this is
        # German — so the shell chrome said "Home" and "Word Processor"
        # in English while the toolbar under it read "Neu / Tabelle /
        # Stile / Schriftart". Two languages in one frame reads as broken
        # software, and every assertion in this file passed while it did:
        # nothing here looks at a word on the screen.
        #
        # A real Tilo install has the OS set this and cannot drift; a
        # session nested on somebody's desktop inherits whatever they
        # use, so the harness pins it. `LANGUAGE` is dropped rather than
        # set, because it outranks `LANG` and is a colon-separated list
        # that would quietly win.
        env.pop("LANGUAGE", None)
        env.update(
            XDG_RUNTIME_DIR=self.runtime_dir,
            XDG_CONFIG_HOME=os.path.join(self.dir, "config"),
            XDG_DATA_HOME=os.path.join(self.dir, "data"),
            XDG_CACHE_HOME=os.path.join(self.dir, "cache"),
            XDG_STATE_HOME=os.path.join(self.dir, "state"),
            # Named outright rather than left to `dirs::config_dir()`.
            # That helper ignores a *relative* XDG_CONFIG_HOME, so a run
            # started with a relative --out gets no isolation from it at
            # all and persists into the real ~/.config/tilo/state.json —
            # three demo sessions writing workspaces, owned by
            # identities nobody at this machine can administer, into
            # somebody's live desktop. `--out` is absolutised for the
            # same reason; this is the belt to that pair of braces.
            LC_ALL="en_US.UTF-8",
            LANG="en_US.UTF-8",
            TILO_STATE_PATH=os.path.join(self.dir, "config", "state.json"),
            TILO_USER_ID=self.user,
            TILO_PEER_KEY_PATH=os.path.join(self.dir, "keys", "peer-key"),
            TILO_FS_ACL_PATH=os.path.join(self.dir, "config", "fs-capabilities.json"),
            TILO_TEST_CONTROL=self.sock,
            TILO_HEADLESS_SIZE=self.size,
            # `info`, plus the two targets that narrate a cross-client
            # drag. Beat 8 aims a press at a file row inside a client,
            # and a client's widgets are not in the compositor's
            # registry — so the only way to know *which row was grabbed*
            # is to have the app say so. It does:
            #
            #   file_explorer::drag  "drag armed"  path=… at=Point{…}
            #   file_explorer::drag  "drag begins" path=…
            #   shell::selection     "client started a drag"
            #
            # Both are `debug!`, so at plain `info` the beat would be
            # aiming blind and could only report on the outcome. An
            # explicit RUST_LOG in the environment still wins, because
            # somebody debugging this wants their own filter.
            RUST_LOG=env.get(
                "RUST_LOG",
                "info,shell::selection=debug,file_explorer::drag=debug,tilo.state=debug"),
            # A home of this session's own, staged with the demo's files.
            HOME=self.home,
            # The two stores an app launch cannot do without. The install
            # root is this session's; the runtime bundle is the machine's,
            # because every packaged app pins it and none of them ship it.
            TILO_APPS_DATA_DIR=self.apps_dir,
            TILO_RUNTIME_BUNDLE_DIR=RUNTIME_BUNDLES,
            # Where the catalogue looks for registries, and where it caches
            # what it fetches. Per-session, so an install in one session is
            # not quietly an install in another.
            TILO_STORE_CONFIG_DIR=os.path.join(self.dir, "store-config"),
            TILO_STORE_CACHE_DIR=os.path.join(self.dir, "store-cache"),
        )
        # The packaging chain, when it is on disk. Passed by path rather
        # than found on PATH so the run measures the binaries built for it.
        for var, path in self.tools.items():
            env[var] = path
        os.makedirs(os.path.join(self.dir, "store-config"), exist_ok=True)
        os.makedirs(os.path.join(self.dir, "store-cache"), exist_ok=True)
        self._log = open(self.log_path, "w")
        argv = [self.binary, "--mode", "native", "--backend", "headless"]
        if self.peer_sync:
            argv.append("--enable-peer-sync")
        self.proc = subprocess.Popen(
            argv,
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
        # The kernel's PDEATHSIG covers the case nothing in userspace can
        # (this process being SIGKILLed). The exit sweep covers the one it
        # does not: a child that outlived an intermediate parent and was
        # reparented. A `--keep` run is asking for survivors and gets no
        # sweep.
        if not KEEP_SESSIONS:
            track(self.proc)

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
        # TILO_TEST_CONTROL on `cfg!(debug_assertions)`, so a shipped binary
        # has no such code and logs nothing about it. Say so, rather than
        # leaving the next person to read a healthy log for an hour.
        alive = self.proc is not None and self.proc.poll() is None
        raise RuntimeError(
            f"{self.name} never answered on {self.sock}"
            + (f"\n  the process is still alive, so it started and did not bind"
               f"\n  the control socket. Is {self.binary} a --release build?"
               f"\n  TILO_TEST_CONTROL is compiled out of release builds by"
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
                     verb: str = "write", resource: str = "document") -> dict:
        """Ask the in-app action gate whether `actor` may invoke `action`
        on `app`'s `resource`.

        Read-only, and the production gate: this is the same check the
        broker runs, asked without performing anything.
        """
        return call(self.sock, {"op": "check_action", "actor": actor,
                                "app": app, "resource": resource,
                                "action": action, "verb": verb})

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

    def pointer_button(self, pressed: bool) -> None:
        """Press or release the left button where the pointer already is.

        `click_at` and `drag` both bundle press and release together,
        which cannot express a drag-and-drop across two clients: the
        source has to still be *holding* the button when it calls
        `start_drag`, the compositor promotes the gesture some frames
        later, and the drop only happens on a release that arrives after
        the pointer has crossed into another window. Bundled ops give
        the source process no wall-clock in which to do any of that.
        These two decompose it (`test_control.rs:198`).
        """
        self.input_ops += 1
        ok(call(self.sock, {"op": "pointer_button", "pressed": pressed}))

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

    def launch(self, app_id: str, timeout: float = LAUNCH_TIMEOUT_S,
               search: str | None = None) -> dict | None:
        """Start an app by its id and wait for it to present a frame.

        The surfaces that already existed are excluded, because every app
        announces the same wayland app-id: a launch that opened nothing
        is otherwise indistinguishable from one that worked, the previous
        window still being on screen and still painting.

        Two routes, and falling from one to the other is a fact about the
        compositor rather than a preference. `actions_invoke` resolves an
        address against a catalogue built with an **empty** query
        (`native.rs:606`), and the applications provider answers an empty
        query with a ranked default set rather than with all of it. On a
        session with forty-odd apps installed most of them are therefore
        unaddressable that way, and the refusal is `unknown_action` — the
        same answer as "no such app", because the gate deliberately will
        not tell a caller what it is not being shown.

        So when the address is refused, the app is opened the way a
        person opens it: the palette, its name typed, the offered row
        activated. Same provider, same launch path, one keystroke instead
        of none — and the route taken is printed, because "opened by
        address" and "opened by typing its name" are different claims and
        only one of them was true.
        """
        before = {f.get("surface_id") for f in self.foreign_frames()}
        resp = call(self.sock, {"op": "actions_invoke",
                                "provider": APPLICATIONS, "id": app_id})
        if "err" in resp:
            code = (resp["err"] or {}).get("code")
            if code != "unknown_action" or search is None:
                note(f"{self.name}: launching {app_id} was refused — "
                     f"{json.dumps(resp['err'])[:160]}")
                return None
            note(f"{self.name}: ({APPLICATIONS}, {app_id}) is not in the "
                 f"catalogue an empty query builds, so it is opened from "
                 f"the palette instead — searching {search!r}")
            self.modifiers(logo=True)
            self.key("Space")
            self.modifiers()
            self.settle(0.4)
            self.type_text(search)
            self.settle(1.2)
            rows = self.palette().get("rows") or []
            if not any(app_id in json.dumps(r) for r in rows):
                note(f"{self.name}: no palette row offers {app_id} for "
                     f"{search!r} (rows: {[r.get('id') for r in rows][:6]})")
                self.key("Escape")
                return None
            self.key("Enter")
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

    def invoke_action(self, provider: str, action_id: str,
                      args: dict | None = None) -> dict:
        """Invoke one action by its stable address. Raw response, so a
        refusal is a result rather than an exception."""
        payload = {"op": "actions_invoke", "provider": provider,
                   "id": action_id}
        if args is not None:
            payload["args"] = args
        return call(self.sock, payload)

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

    def key_down(self, name: str) -> None:
        """Press and hold a key. Pairs with [`key_up`]."""
        self.input_ops += 1
        ok(call(self.sock, {"op": "key_down", "key": name}))

    def key_up(self, name: str) -> None:
        self.input_ops += 1
        ok(call(self.sock, {"op": "key_up", "key": name}))

    def type_prose(self, text: str, pace: bool | None = None) -> None:
        """Type `text` into a *client* so that it arrives as written.

        `type` alone does not. It emits one keycode per character, and a
        client resolves a keycode through the seat's xkb state — which
        has no shift held — so every capital arrives lowercase and every
        shifted symbol arrives as the character below it. Compositor
        chrome is unaffected: it takes the character directly, which is
        why the same call produces `Launch Plan: ok!` in the palette and
        `launch plan; ok1` in an app, and why this was believed for a
        while to be a limitation of the `type` op rather than of the
        path it was being used on.

        The remedy is a real held modifier: `key_down("Shift")` drives
        the seat the way hardware does, and the client's own xkb state
        then resolves the keycode to the shifted symbol. Measured in the
        word processor, not assumed.

        Newlines are `Enter`, which does break a paragraph — the word
        processor starts a new one and even turns a leading `- ` into a
        real bullet. A draft typed this way arrives as a document rather
        than as one lowercase run-on line.

        One character at a time, at a pace a person reads as typing.

        Batching the unshifted runs into a single `type` call is much
        cheaper — one round trip per capital rather than per character —
        and it is the wrong thing on camera. A sentence that arrives in
        three instant bursts is not a person writing; the viewer sees
        text appear and has no idea it was typed, which is the one thing
        this beat exists to show.

        The cadence is deterministic rather than random, so two runs of
        the film type identically: a base interval, plus a longer pause
        after the punctuation and spaces where a person's hands actually
        slow down. Off camera the pause is skipped entirely and the old
        batching applies, because a harness waiting out a realistic
        typing speed is just a slower harness.
        """
        paced = RECORDER is not None and pace is not False
        if not paced:
            pending: list[str] = []

            def flush() -> None:
                if pending:
                    self.type_text("".join(pending))
                    pending.clear()

            for ch in text:
                if ch == "\n":
                    flush()
                    self.key("Enter")
                elif ch in SHIFTED_KEYS:
                    flush()
                    self.key_down("Shift")
                    self.key(SHIFTED_KEYS[ch])
                    self.key_up("Shift")
                else:
                    pending.append(ch)
            flush()
            return

        for ch in text:
            if ch == "\n":
                self.key("Enter")
            elif ch in SHIFTED_KEYS:
                self.key_down("Shift")
                self.key(SHIFTED_KEYS[ch])
                self.key_up("Shift")
            else:
                self.type_text(ch)
            base = TYPING_PAUSE.get(ch, TYPING_PACE)
            time.sleep(base * next(self._typing_jitter))

    def modifiers(self, ctrl=False, shift=False, alt=False, logo=False) -> None:
        self.input_ops += 1
        ok(call(self.sock, {"op": "modifiers", "ctrl": ctrl, "shift": shift,
                            "alt": alt, "logo": logo}))

    # -- the clipboard (how a token crosses machines) ----------------

    def clipboard_read(self) -> str | None:
        """What this session's clipboard holds."""
        return (call(self.sock, {"op": "clipboard_paste"})
                .get("clipboard") or {}).get("text")

    def clipboard_write(self, text: str) -> None:
        """Put `text` on this session's clipboard.

        Used for the hop between two machines, which is a person's job —
        a chat message, an email, a USB stick — and not the OS's. What
        travels is taken from the *issuing* session's clipboard, so a
        copy that silently truncated is a failure here rather than a
        token that happens to work because the harness had it anyway.
        """
        ok(call(self.sock, {"op": "clipboard_copy", "text": text}))

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
        self.key("Space")
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

# The lower third. The name of the session and what it is, over a scrim
# dark enough to stay legible against whatever it is sitting on — and
# nothing else. The transcript belongs in the terminal; on the video it
# was a wall of text nobody reads while a screen is moving underneath.
BAND_H = 66

# What each session is recorded at: the video's size *minus the band*.
#
# The band used to be drawn over the frame, and a compositor laying out
# for the full 1080 put the bottom of every window under it — a status
# bar, a word count, the foot of a file list, clipped in every shot that
# mattered. Nothing in the compositor is wrong there; the camera was
# covering a screen it had asked to be full height.
#
# So the session is given the height the video can actually show, and
# the band occupies the strip below it. A frame still goes in 1:1 with
# nothing resampled, and no pixel the compositor drew is hidden.
RECORD_SIZE = f"{VIDEO_W}x{VIDEO_H - BAND_H}"
BAND_SCRIM = "black@0.72"
BAND_PAD_X = 52

BG = "0x0B0D12"       # the shell's own backdrop, for any letterboxing
FG = "0xE9ECF2"

# Who each session is, in the video. With one screen on at a time the
# band is the only thing telling you whose screen you are looking at, so
# it carries a name and a standing in the room, and nothing else.
#
# The third one used to read "driven only by (provider, id)", which is
# true and is the wrong thing to put on a screen: it is the name of a
# mechanism, shown to people who have never seen this system. How it is
# driven is what the beat demonstrates and what the transcript proves.
# On the band it is simply another participant.
ROLES = {
    "ada": "hosts the room",
    "bob": "joined by invite",
}

# The one session this film never shows, and the reason it has a name of
# its own rather than an absent entry in ROLES.
#
# The claim being filmed is that an agent never has to look at a screen —
# it reads the document, and what it did shows up on the other two
# desktops the way anybody else's edit does. A shot of *its* desktop
# argues against that in the one frame meant to prove it, and it is an
# easy shot to take by accident: the session is right there beside the
# other two and answers `snapshot` exactly like them.
#
# So it is refused twice. `cut` will not accept the name, and the run
# fails at the end if a single frame turns out to have come from it —
# because the second check is the one that holds when somebody adds a
# capture path that does not go through `cut`.
NEVER_ON_CAMERA = "Claude"

# How long a frame the viewer has to *read* — a token, a name in a
# composer, a card saying it has been copied — stays up.
#
# Long by the standards of the rest of the cut, and deliberately immune
# to `--record-max-hold`: that cap exists to throw away a session sitting
# on an unchanged screen while it waits for a sandbox, and a card nobody
# has had time to read is the opposite case. Frames taken inside `hold`
# are marked, and trimming skips them. See `Recorder._timeline`.
READ_HOLD_S = 2.6

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
# against so a contended run says so instead of looking like a slow Tilo.
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
        # The share of the last snapshot's cost handed back to the
        # compositor before taking the next one. Raised, per beat,
        # around anything this film measures — see `easing`.
        self.yield_share = CAPTURE_YIELD
        self.dwell_s = dwell
        self.cut_dwell_s = cut_dwell
        # The longest one unchanged frame is shown for. See `_timeline`.
        self.max_hold = max_hold
        # How much still picture the cap took out, filled in at compose.
        self.trimmed = 0.0
        # (taken_at, relative path, session name, deliberately held) in
        # capture order. The last field marks a frame taken while the
        # demo was holding on purpose, which is what exempts it from the
        # still-picture trim.
        self.frames: list[tuple[float, str, str, bool]] = []
        # How long each snapshot blocked its session. The honest measure
        # of what the camera costs, reported rather than estimated.
        self.blocked: list[float] = []
        self.errors: list[str] = []
        self._failed: set[str] = set()
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None
        self._live: Session | None = None
        # Who the *narrative* is on, which is not the same as who the
        # camera is on: `hold_off` takes the camera down for a beat that
        # is run and proven but not filmed, and `_wanted` is what it
        # comes back up on.
        self._wanted: Session | None = None
        self._off = 0
        self._holding = False
        self._lock = threading.Lock()
        self.started_at = 0.0
        self.stopped_at = 0.0

    # -- capture -----------------------------------------------------

    def start(self, first: str) -> None:
        os.makedirs(os.path.join(self.dir, "frames"), exist_ok=True)
        self._live = self._wanted = self.by_name[first]
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
            held = self._holding
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
            self.frames.append((began, rel, s.name, held))
            index += 1
            self._stop.wait(max(MIN_CAPTURE_GAP_S, self.yield_share * took,
                                self.interval - took))

    # -- getting out of the way --------------------------------------

    @contextlib.contextmanager
    def easing(self, share: float):
        """Take the camera off the compositor's back for a moment.

        A snapshot of a 1920x1080 surface costs this build ~0.14s, and
        the pacing floor is 0.04s, so the filmed compositor spends about
        78% of its event loop inside `snapshot`. That is fine for
        watching, and wrong for anything the demo *measures*: an install
        timed through the camera came out at 21s against a real median
        of 7.8s, and a viewer with a stopwatch would have read the
        camera's overhead as the product's speed.

        So a measured beat asks for a larger share of the loop to be
        left alone. It still films — a beat nobody can see is worse than
        a slow number — just at a lower frame rate for its duration.
        """
        was = self.yield_share
        self.yield_share = share
        try:
            yield
        finally:
            self.yield_share = was

    # -- the cut -----------------------------------------------------

    def cut_to(self, name: str, dwell: float | None = None) -> None:
        """Put one session on camera, and hold there long enough to read
        it before the demo moves on.

        Cutting to the session already on camera is a no-op, so the beat
        functions can name their subject without inserting a pause that
        the narrative did not ask for.
        """
        if name == NEVER_ON_CAMERA:
            raise RuntimeError(
                f"{name} is never on camera: an agent with a desktop is the "
                f"one thing this film must not show. See NEVER_ON_CAMERA.")
        with self._lock:
            if self._wanted is not None and self._wanted.name == name:
                return
            self._wanted = self.by_name[name]
            # Noted while the camera is down, and acted on when it comes
            # back up. No dwell either: nobody is being moved anywhere.
            if self._off:
                return
            self._live = self._wanted
        hold = self.cut_dwell_s if dwell is None else dwell
        if hold > 0:
            time.sleep(hold)

    @contextlib.contextmanager
    def hold_off(self):
        """Run a beat without putting it in the film.

        Every beat in this demonstration is run and proven; not every one
        of them belongs in a one-minute cut. A beat filmed and then
        thrown away costs the sessions the snapshot time anyway — and
        snapshotting is most of the filmed compositor's event loop — so
        the camera comes down instead and the beat runs at full speed.

        This is an edit, and the same kind as a cut: what it can do is
        leave material out. It cannot put anything in, reorder anything,
        or make a frame show for longer than its session held it — the
        frame before a pause is capped by `max_hold` like any other.
        """
        with self._lock:
            self._off += 1
            self._live = None
        was = self._live
        try:
            yield
        finally:
            changed = False
            with self._lock:
                self._off -= 1
                if not self._off:
                    self._live = self._wanted
                    changed = self._live is not was
            # A beat run off camera can name a different session for what
            # follows. Coming back up on one is a cut like any other and
            # is paid for like one, or the film jumps between two desktops
            # with no beat in between to land on.
            if changed and self.cut_dwell_s > 0:
                time.sleep(self.cut_dwell_s)

    @contextlib.contextmanager
    def patience(self, seconds: float):
        """Film the first `seconds` of a wait, then stop filming it.

        The still-picture trim cannot see a wait for what it is. It
        compares frames byte for byte, and a screen with a caret blinking
        in it is a different screen every half second — so a minute spent
        waiting for something to arrive is a minute of *changing* frames
        and survives the cap in full. One run of this film spent 180 of
        its 318 seconds on two waits that had already failed, and every
        frame in them was legitimately different from the last.

        So the wait is bounded by the camera rather than by the trim.
        What arrives quickly is filmed arriving; what does not is not sat
        through. The bound is generous enough to cover the thing working
        — a viewer sees the result appear — and is only ever reached when
        it is not.
        """
        stopped = threading.Event()
        dropped = threading.Event()

        def drop_after():
            if not stopped.wait(seconds):
                with self._lock:
                    self._off += 1
                    self._live = None
                dropped.set()

        waiter = threading.Thread(target=drop_after, name="patience",
                                  daemon=True)
        waiter.start()
        try:
            yield
        finally:
            stopped.set()
            waiter.join(timeout=5)
            if dropped.is_set():
                print(f"     .   [ camera came down after {seconds:.0f}s of "
                      f"waiting ]", flush=True)
                with self._lock:
                    self._off -= 1
                    if not self._off:
                        self._live = self._wanted

    @contextlib.contextmanager
    def holding(self):
        """Mark what is filmed in here as a deliberate hold.

        Frames taken inside are exempt from `max_hold`, so a card the
        demo stopped on to be read survives the trim that removes a
        session waiting on a sandbox spawn. Both are an unchanging
        screen; only one of them is dead air.
        """
        was = self._holding
        self._holding = True
        try:
            yield
        finally:
            self._holding = was

    def hold(self, seconds: float) -> None:
        """Stop here, on camera, for `seconds`. Really elapsed, really
        sampled — and kept whole by the trim.

        Nothing at all when the camera is down. A hold buys a viewer time
        to read something, and an unfilmed beat has no viewer: paying it
        anyway would add half a minute of wall clock to a run for frames
        that do not exist.
        """
        if seconds <= 0 or self._off:
            return
        with self.holding():
            time.sleep(seconds)

    def camera_is_down(self) -> bool:
        return self._off > 0

    def frames_from(self, name: str) -> int:
        return sum(1 for _t, _rel, n, _h in self.frames if n == name)

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
        for _t, _rel, name, _held in self.frames:
            per[name] = per.get(name, 0) + 1
        mean_ms, worst_ms = self.snapshot_ms()
        print(f"\ncaptured over {span:.1f}s of demo: {len(self.frames)} frames, "
              f"{self.rate():.2f} frames/s")
        print(f"  snapshot blocked its session {mean_ms:.0f} ms on average "
              f"(worst {worst_ms:.0f} ms)")
        # The frame rate of this video *is* the compositor's snapshot
        # speed, so a recording made on a busy machine measures the rest
        # of the machine. Worth saying outright: the number above is
        # otherwise indistinguishable from Tilo being slow.
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

    def beat_report(self, marks: list[tuple[str, float]], fps: int) -> None:
        """How many seconds of the finished video each beat accounts for.

        Not wall clock: the sum of the screen time actually given to the
        frames captured during that beat, after trimming and after any
        stretch the camera was down for. So it answers the only question
        that matters when a cut is too long — which beat to take it out
        of — instead of inviting somebody to guess.
        """
        if not marks or not self.frames:
            return
        t_start, t_end = self._window()
        timeline = self._timeline(t_start, t_end, fps)
        # `_timeline` collapses runs, so walk the frames it kept and
        # charge each one's screen time to the beat it was taken in.
        kept = {rel: seconds for rel, _name, seconds in timeline}
        edges = [t for _title, t in marks] + [float("inf")]
        spend = [0.0] * len(marks)
        for taken, rel, _name, _held in self.frames:
            if rel not in kept:
                continue
            for i in range(len(marks)):
                if edges[i] <= taken < edges[i + 1]:
                    spend[i] += kept[rel]
                    break
        total = sum(spend)
        print("  of the finished video:")
        for (title, _t), seconds in zip(marks, spend):
            share = 100.0 * seconds / total if total else 0.0
            bar = "#" * int(round(share / 3))
            print(f"    {seconds:6.1f}s  {share:4.1f}%  {bar:<12} "
                  f"{title[:52]}")

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

        # How long each frame was the live state, before any editing.
        lived: list[float] = []
        for i, (t, _rel, _name, _held) in enumerate(frames):
            begins = t_start if i == 0 else t
            # The last frame was the live state until the camera stopped;
            # it is shown for one tick and the video ends.
            ends = frames[i + 1][0] if i + 1 < len(frames) else t_end + tick
            lived.append(max(tick, ends - begins))

        # Runs of *identical* frames are the dead air, and capping a
        # single frame's screen time never touched them: the camera takes
        # a fresh snapshot every fifth of a second, so a minute of an
        # unchanging screen arrives as three hundred separate files that
        # each lived a fifth of a second and none of which was ever held.
        # A previous cut trimmed one second out of two hundred and forty
        # while a third of it was a still page.
        #
        # So identical neighbours are collapsed: one of them is kept and
        # shown for at most `max_hold`, and the rest are dropped. That
        # takes frames out and never puts any in — nothing is repeated,
        # nothing is interpolated, and every frame in the file is still a
        # real render shown for no longer than its run was live.
        runs: list[tuple[int, int]] = []          # (index, count)
        digests = [self._digest(rel) for _t, rel, _n, _h in frames]
        i = 0
        while i < len(frames):
            j = i + 1
            # A run has to stay on one session: a cut is a change even
            # when the two screens happen to be identical.
            while (j < len(frames)
                   and digests[j] == digests[i]
                   and digests[i] is not None
                   and frames[j][2] == frames[i][2]
                   # A hold that begins part-way through an unchanging
                   # screen starts its own run, so the cap lifts exactly
                   # where the demo asked it to.
                   and frames[j][3] == frames[i][3]):
                j += 1
            runs.append((i, j - i))
            i = j

        for start, count in runs:
            _t, rel, name, held = frames[start]
            was_live = sum(lived[start:start + count])
            # A frame the demo deliberately stopped on is shown for as
            # long as it was live. The cap is for the other kind of still
            # picture — a session waiting — and cannot tell the two apart
            # by looking at the pixels, so it is told.
            shown = was_live if held else min(was_live, max(tick, cap))
            self.trimmed += was_live - shown
            out.append((rel, name, shown))
        return out

    def _digest(self, rel: str) -> str | None:
        """A frame's bytes, as a hash, or None when it cannot be read.

        The compositor encodes the same scene to the same PNG, so equal
        bytes mean an identical screen. An unreadable frame gets None,
        which compares unequal to everything including itself, so a
        frame that could not be hashed is never collapsed into a run.
        """
        try:
            with open(os.path.join(self.dir, rel), "rb") as f:
                return hashlib.sha1(f.read()).hexdigest()
        except OSError:
            return None

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
        # The band sits *below* the screen rather than over it. A session
        # recorded at the video's height minus the band goes in 1:1 and
        # is padded downwards, so the strip the band occupies is empty
        # canvas and no window is ever covered by it. See RECORD_SIZE.
        if (source_w, source_h) == (VIDEO_W, VIDEO_H - BAND_H):
            chain.append(f"pad={VIDEO_W}:{VIDEO_H}:0:0:color={BG}")
        # No scale filter at all when the session already renders at the
        # video's size, which is what `--record-size` picks by default.
        # Resampling a 1:1 frame would cost exactly the legibility that
        # recording full-frame was for.
        elif (source_w, source_h) != (VIDEO_W, VIDEO_H):
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
            # Capitalised on the band, lowercase everywhere else. The
            # session identity really is `ada` — it is a unix user,
            # and the roster, the room document and every log line spell
            # it that way. But this band is prose about a person, in a
            # film, and a person's name has a capital in it. Leaving the
            # login on screen reads as an unfinished caption rather than
            # as fidelity to the system underneath.
            who = name[:1].upper() + name[1:]
            text = f"{who} · {ROLES[name]}" if name in ROLES else who
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
# (title, wall clock when it started). Only so the report can say how
# many seconds of the finished video each beat accounts for — which is
# the question every attempt to shorten this film has had to answer by
# guessing. See `Recorder.beat_report`.
BEAT_MARKS: list[tuple[str, float]] = []

RULE = "-" * 70

# Set by `--record`. The transcript is the demo's narration, and it stays
# in the terminal: on the video the beats and claims were a wall of text
# competing with the screen underneath, so what the recording takes from
# these calls is timing — how long to hold — and nothing else.
RECORDER: Recorder | None = None


@contextlib.contextmanager
def easing(share: float = 4.0):
    """Film a measured beat without the camera in its numbers."""
    if RECORDER is None:
        yield
        return
    with RECORDER.easing(share):
        yield


def cut(name: str) -> None:
    """Put `name` on camera for what follows.

    Cuts are expensive in a way that has nothing to do with what they
    cost to make: every one of them asks a viewer who has never seen this
    system to work out whose screen they are now looking at, and to hold
    two machines in their head at once. So this cut goes to bob twice —
    once to watch him redeem the invitation, once at the end for what
    arrived on his screen without him doing anything — and is on ada
    for everything in between, including the beats that are *about*
    bob. Driving a session and filming it are separate: the far side
    can be opened, joined and read while the camera stays where the story
    is.

    Printed either way, so the transcript documents the intended order of
    shots even on a run that is not being recorded.
    """
    if name == NEVER_ON_CAMERA:
        raise RuntimeError(
            f"{name} is never on camera — see NEVER_ON_CAMERA. What the "
            f"agent does is shown on the screens of the people it does it "
            f"with, which is the claim; its own desktop is the one shot "
            f"that would contradict them.")
    # Said differently while the camera is down, because it is a
    # different fact: nothing is being shown to anybody, and all this
    # does is decide what the film comes back up on.
    down = RECORDER is not None and RECORDER.camera_is_down()
    print(f"\n  [ {'next up' if down else 'on camera'}: {name} ]", flush=True)
    if RECORDER:
        RECORDER.cut_to(name)


@contextlib.contextmanager
def off_camera(why: str):
    """Run what follows without filming it.

    For a beat that has to happen — because a later one reads its result,
    or because it is part of what this run proves — and does not belong
    in a one-minute cut. It runs at full speed, every claim in it is
    asserted and printed, and none of it reaches the video.
    """
    print(f"\n  [ camera off — {why} ]", flush=True)
    if RECORDER is None:
        yield
        return
    with RECORDER.hold_off():
        yield
    print("\n  [ camera back on ]", flush=True)


@contextlib.contextmanager
def patience(seconds: float):
    """Give a wait this long on camera, and no longer.

    For the waits that are worth watching succeed and are not worth
    watching fail: an agent carrying out an instruction, a document
    crossing a network. See `Recorder.patience`.
    """
    if RECORDER is None:
        yield
        return
    with RECORDER.patience(seconds):
        yield


def read_hold(seconds: float = READ_HOLD_S) -> None:
    """Stop on what is on screen, long enough to read it.

    For the frames that carry words a viewer is supposed to take in — a
    token, the name an invitation was made out to, a card saying it is on
    the clipboard. Real elapsed time, sampled like everything else, and
    exempt from the still-picture trim so the hold survives into the cut.
    """
    if RECORDER:
        RECORDER.hold(seconds)


def banner(title: str) -> None:
    print(f"\n{RULE}\n  {title}\n{RULE}", flush=True)
    BEAT_MARKS.append((title, time.time()))
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


class LogTail:
    """A session's own log, read as it is written.

    Everything a cross-client drag does is narrated here by the two
    processes doing it, and nowhere else a harness can reach: the file
    manager's rows are not in the compositor's widget registry, the
    compositor's drag label is not in `state_dump`, and the word
    processor's document is not on screen in any form a script can read.
    So this is not a shortcut around an API — it is the API, and every
    line below is one of the two processes reporting on itself.

    The app's stderr reaches this file because the launcher pipes a
    sandboxed app's output into the session's, which is also how
    `[app] broadcast doc delta` has always been readable.
    """

    #: The file manager, on a press that landed on a row. Names the path
    #: and the press position *in the client's own coordinates*, which
    #: together are enough to convert a miss into a correction rather
    #: than a re-guess.
    #: `at` is a `kurbo::Point`, whose `Debug` is `(x, y)` — *not*
    #: `Point { x: …, y: … }`, which is what this matched first and
    #: would never have found.
    ARMED = re.compile(
        r'drag armed\s+path="([^"]+)"\s+at=\((-?[\d.]+),\s*(-?[\d.]+)\)')
    #: The same press, once the pointer has travelled far enough.
    BEGAN = re.compile(r'drag begins\s+path="([^"]+)"')
    #: The app handing the gesture to the compositor — the moment it
    #: stops being an in-app drag and becomes a desktop-wide one.
    CARRIED = "carrying the drag out of this app as a client drag"
    #: The compositor, having accepted it.
    PROMOTED = "client started a drag"
    #: The compositor, on the drop.
    FINISHED = "drag finished"
    #: A `ui` app reporting the size of a change to its own document.
    DELTA = re.compile(r"broadcast doc delta (\d+)B")

    #: `tracing_subscriber::fmt::init()` colours its output whether or
    #: not the sink is a terminal, and the session's log is a file. So a
    #: field arrives as `path\x1b[0m\x1b[2m=\x1b[0m"…"` rather than
    #: `path="…"`, and a pattern written against what the line *reads*
    #: as matches nothing at all. Cost a whole run to find; stripped
    #: here, once, so no pattern below has to know about it.
    ANSI = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")

    def __init__(self, path: str) -> None:
        self.path = path
        self.at = 0
        self.text = ""

    def read(self) -> str:
        """Everything appended since the last read, decoloured."""
        try:
            with open(self.path, "rb") as fh:
                fh.seek(self.at)
                chunk = fh.read()
                self.at = fh.tell()
        except OSError:
            return ""
        fresh = self.ANSI.sub("", chunk.decode(errors="replace"))
        self.text += fresh
        return fresh

    def catch_up(self) -> None:
        """Skip whatever is already there without remembering it, so
        setup is never mistaken for the gesture."""
        self.read()
        self.text = ""

    def armed(self, fresh: str) -> tuple[str, float, float] | None:
        """The path and client-space point of the last press that landed
        on a row, or None if none did."""
        hits = self.ARMED.findall(fresh)
        if not hits:
            return None
        path, x, y = hits[-1]
        return path, float(x), float(y)

    def biggest_delta(self, fresh: str) -> int:
        return max((int(m) for m in self.DELTA.findall(fresh)), default=0)

    def await_line(self, needle: str, timeout: float) -> bool:
        """Wait for `needle` to appear, reading as it goes."""
        deadline = time.time() + timeout
        while time.time() < deadline:
            if needle in self.read() or needle in self.text:
                return True
            time.sleep(0.1)
        return False


def wait_until_painted(s: Session, timeout: float = 45.0) -> list[str]:
    """Wait until the shell is compositing every client window it holds.

    `frame_seq > 0` says the client produced a frame; `presented` says
    the *shell* is painting it. The two are seconds apart on a loaded
    machine, and in between the compositor answers `list_targets` and
    `foreign_frames` with laid-out rects for windows that do not appear
    in a snapshot at all — so a rig that trusts the rects stages a scene,
    photographs an empty desktop, and reports success. Seen twice.

    Returns the app ids still unpainted, so a caller can say which one
    rather than only that something was.
    """
    def unpainted() -> list[str]:
        return [str(f.get("app_id")) for f in s.foreign_frames()
                if not f.get("presented")]

    deadline = time.time() + timeout
    while time.time() < deadline:
        missing = unpainted()
        if not missing:
            return []
        time.sleep(0.5)
    return unpainted()


def painted_tiles(s: Session) -> dict[int, dict]:
    """Every client window this session is painting, by surface id.

    Off `foreign_frames`, which reports the rect the widget tree laid
    each tab into — the placement the compositor is actually painting.
    That matters more than it sounds: the same tile is registered in the
    widget registry under *two* names (`foreign-surface-<id>` and
    `foreign-surface-<id>[<tab>]`), so an exact-name lookup can come back
    empty while the window is plainly on screen. This has no names in it
    to get wrong.
    """
    out: dict[int, dict] = {}
    for f in s.foreign_frames():
        r = f.get("rect") or {}
        if not (r.get("w") and r.get("h")):
            continue
        sid = f.get("surface_id")
        if sid is None:
            continue
        out[int(sid)] = {
            "app_id": f.get("app_id"),
            "rect": (r["x"], r["y"], r["w"], r["h"]),
            "frame_seq": f.get("frame_seq"),
            "presented": f.get("presented"),
        }
    return out


def windows_off_screen(s: Session, viewport: tuple[int, int]) -> list[str]:
    """Which client windows are not wholly on screen, and by how much.

    Read off the compositor's own per-surface rects rather than off a
    picture, because "the left window is cut in half" and "the left
    window is behind the right one" look similar in a still and are
    different faults. A window that is not fully in frame is named here
    so a beat can refuse to claim what its own shot does not show.
    """
    w, h = viewport
    out = []
    for f in s.foreign_frames():
        r = f.get("rect") or {}
        x0, y0 = r.get("x", 0.0), r.get("y", 0.0)
        x1, y1 = x0 + r.get("w", 0.0), y0 + r.get("h", 0.0)
        over = []
        if x0 < -0.5:
            over.append(f"{-x0:.0f}px off the left")
        if x1 > w + 0.5:
            over.append(f"{x1 - w:.0f}px past the right")
        if y1 > h + 0.5:
            over.append(f"{y1 - h:.0f}px below the screen")
        if over:
            out.append(f"{f.get('app_id')} — {', '.join(over)}")
    return out


#: Height of the compositor's own top bar, in the same coordinates
#: `foreign_frames` reports tiles in. A click above this line reaches the
#: shell's chrome rather than any window, so it is the floor for anything
#: this file aims at a tab pill.
TOPBAR_H = 44.0


def windows_overlapping(s: Session) -> list[str]:
    """Which client windows are drawn on top of each other.

    The other half of "is this scene filmable", and the half nothing was
    checking. `windows_off_screen`'s own docstring says that "the left
    window is cut in half" and "the left window is behind the right one"
    look similar in a still and are different faults — and then it only
    detects the first. So a session painting one window full-screen
    across another passed "both windows are wholly on screen before the
    gesture", and the drag that followed was aimed into a stack: both
    tiles were inside the viewport, and one was underneath the other.

    A one-pixel seam between neighbouring tiles is not an overlap, so
    this is measured as a share of the smaller window's area rather than
    as any intersection at all.
    """
    tiles = [(str(t["app_id"]), t["rect"]) for t in painted_tiles(s).values()]
    out = []
    for i in range(len(tiles)):
        for j in range(i + 1, len(tiles)):
            (a_id, (ax, ay, aw, ah)) = tiles[i]
            (b_id, (bx, by, bw, bh)) = tiles[j]
            ox = max(0.0, min(ax + aw, bx + bw) - max(ax, bx))
            oy = max(0.0, min(ay + ah, by + bh) - max(ay, by))
            area = ox * oy
            smaller = min(aw * ah, bw * bh)
            if smaller > 0 and area / smaller > 0.05:
                out.append(f"{a_id} and {b_id} overlap by "
                           f"{100.0 * area / smaller:.0f}% of the smaller")
    return out


def frame_both_windows(s: Session, viewport: tuple[int, int] | None = None) -> None:
    """Put the whole strip in frame: even shares, and scrolled back.

    Two separate things go wrong, and each has to be checked rather than
    performed and assumed.

    A group takes about three quarters of the strip by default, which is
    right for reading one window and wrong for filming two — so every
    group is given an even share. Even means *even between however many
    groups there are*, which is the half that was wrong: a fixed half
    each is a strip one and a half screens wide the moment a third group
    exists, and the window on the end then hangs off by most of its own
    width. That is what `788px past the right` was, printed underneath a
    claim that both windows were whole.

    The second is the strip's scroll offset. Opening a window in a new
    group scrolls the strip to bring *that* group up, and it does not
    come back when the content would fit whole: measured at 1920 wide,
    two even groups land at `-222 … 578` and `586 … 1386`, so the first
    window is cut by 222px on the left while 534px of empty backdrop
    sits on the right. Narrowing the groups does not help on its own —
    the offset stays at -222 and only the windows get smaller — and
    scrolling the backdrop does not move it either.

    Focusing the leftmost group does move it: the same two windows then
    sit at `291 … 1091` and `1099 … 1899`, both whole, filling the strip
    beside the wheel disc. So that is the gesture — a click on the
    window you have just opened something next to, which is what a
    person does anyway.

    Then it is measured, and narrowed and re-framed until it holds or the
    attempts run out. A beat that needs two windows in one shot cannot
    take "the gesture was performed" for an answer.
    """
    groups = max(1, len(s.groups_here()))
    attempts = [1000 // groups, 900 // groups, 800 // groups]
    for share in attempts:
        share = max(120, min(1000, share))
        resp = s.set_group_width(share)
        if "err" in resp:
            note(f"{s.name}: could not even out the strip — "
                 f"{json.dumps(resp['err'])[:120]}")
            break
        s.settle(0.6)
        focus_leftmost_group(s)
        if not viewport:
            return
        wrong = windows_off_screen(s, viewport) + windows_overlapping(s)
        if not wrong:
            note(f"{s.name}: {groups} group(s) at {share} permille of the "
                 f"strip — every window is wholly on screen and none is "
                 f"behind another")
            return
        note(f"{s.name}: not framed yet at {share} permille "
             f"({'; '.join(wrong)}) — narrowing and re-framing")

    if viewport:
        wrong = windows_off_screen(s, viewport) + windows_overlapping(s)
        if wrong:
            gap(f"{s.name}: the strip is not filmable after "
                f"{len(attempts)} attempt(s) at framing it — "
                + "; ".join(wrong))
        else:
            note(f"{s.name}: every window is wholly on screen and none is "
                 f"behind another")


def focus_leftmost_group(s: Session) -> None:
    """Click the leftmost client tile's tab pill, which scrolls the strip
    back to it.

    Aimed at the pill rather than into the window's body: a click in the
    body reaches the client and selects whatever is under it, and a file
    row highlighted by the camera is the camera making the picture rather
    than the demo.

    A single tile is already the whole strip, so there is nothing to
    scroll back to and the click is skipped — it would only take focus
    off whatever the beat had just put it on.

    The tiles come from `painted_tiles`, not from the widget registry.
    The registry names the same tile two ways (`foreign-surface-<id>` and
    `foreign-surface-<id>[<tab>]`) and does not always carry it the
    instant the window is laid out — so a lookup by name could report one
    tile while two were on screen, conclude there was nothing to scroll
    back to, and leave the second window hanging off the right edge.
    `foreign_frames` reports the rect the widget tree laid each tab into
    and has no names in it to get wrong.
    """
    tiles = [(t["rect"][0], str(t["app_id"]), t["rect"])
             for t in painted_tiles(s).values()]
    if len(tiles) < 2:
        return
    tiles.sort()
    _x, name, rect = tiles[0]
    x, y, w, _h = rect
    # The pill sits above the tile's top edge; 18px up lands on it and
    # stays clear of the client's own first row.
    #
    # Unless the tile starts at the top of the screen, which is a window
    # occupying the whole strip and wearing no pill at all — 18px above
    # *that* is the compositor's own top bar, and the click lands on the
    # workspace switcher. A gesture meant to scroll a strip must never be
    # able to change which workspace is on camera, so it is skipped and
    # said out loud rather than aimed anyway.
    py = y - 18.0
    if py < TOPBAR_H:
        note(f"{s.name}: the leftmost window ({name}) starts at y={y:.0f}, "
             f"so it wears no tab pill to click — the strip is not scrolled "
             f"back")
        return
    s.click_at(max(4.0, x + min(90.0, w / 2.0)), py)
    s.settle(0.8)
    note(f"{s.name}: focused the left group ({name}) so the strip scrolls "
         f"back and both windows are in frame")


def frame_one_window(s: Session, app_id: str, prefer: dict | None = None,
                     share: int = 720) -> None:
    """Put one window up at a readable width, with the strip around it.

    `frame_both_windows` exists for a beat that has to show two windows
    at once, and it pays for that: three groups get a third of the strip
    each, and a third of this strip is a word processor whose styles
    panel leaves a column of page about as wide as a phone. The last shot
    of the film is about what is *written* in a document, and it came out
    with the text clipped mid-word.

    A beat about one window asks for the opposite: that window at close
    to the width a person would give it, with its neighbours where a
    scrollable strip actually leaves them — visible at the edges, not
    squared off into equal thirds. Which is also the more honest picture
    of the layout model, and the one the strip is for.
    """
    # Scrolled to first, then widened, then scrolled to again. Widening
    # moves every tile, and a tile that has been pushed off the edge has
    # no pill on screen to click — so the strip is brought to it while
    # everything is still small enough to be in view, and confirmed
    # afterwards.
    focus_app_group(s, app_id, prefer=prefer)
    resp = s.set_group_width(share)
    if "err" in resp:
        note(f"{s.name}: could not widen the strip — "
             f"{json.dumps(resp['err'])[:120]}")
    s.settle(0.6)
    focus_app_group(s, app_id, prefer=prefer)
    # Measured, not assumed — the same rule the two-window framing keeps.
    # Focusing a group scrolls it *into view*, which is not the same as
    # scrolling it *wholly* into view: at 720 permille the shot came out
    # with the document readable and its right edge past the screen.
    # Said rather than silently accepted, because the next person to
    # widen this will otherwise re-find it from the frame.
    if VIEWPORT:
        clipped = [w for w in windows_off_screen(s, VIEWPORT) if app_id in w]
        if clipped:
            note(f"{s.name}: the framed window is not wholly on screen — "
                 + "; ".join(clipped))


#: How many pixels have to move before a screen counts as having
#: changed. A text caret blinking on and off is about fifty; a remote
#: peer's name pill is a bit over a thousand; a line of text arriving in
#: a document is tens of thousands. Set above the second and far below
#: the third, so a page that gained a sentence is never confused with a
#: page that only blinked.
SCREEN_MOVED_PX = 4000


def screen_shot(s: Session, where: str) -> str | None:
    """Put what `s` is painting on disk, and hand back the path."""
    try:
        s.snapshot(where)
        return where
    except Exception as exc:  # noqa: BLE001
        note(f"{s.name}: could not read the screen back ({exc})")
        return None


def pixels_moved(before: str | None, after: str | None) -> int | None:
    """How many pixels differ between two shots, or None if it could not
    be worked out.

    The one question a socket cannot answer: did that screen actually
    change. Comparing the PNGs byte for byte is not it — the two shots
    either side of this beat came out the same size and different bytes,
    because a caret had blinked, so a hash says "changed" about a screen
    on which nothing happened. This decodes and counts instead.
    """
    if not before or not after:
        return None
    try:
        from PIL import Image, ImageChops
    except Exception:                       # noqa: BLE001
        note("PIL is not available, so the far screen could not be "
             "compared against what its session answered")
        return None
    try:
        with Image.open(before) as a, Image.open(after) as b:
            a, b = a.convert("RGB"), b.convert("RGB")
            if a.size != b.size:
                return None
            grey = ImageChops.difference(a, b).convert("L")
            # A threshold, not an exact match: subpixel text rendering
            # moves a value by one or two all over a page that has not
            # changed at all.
            moved = grey.point(lambda v: 255 if v > 24 else 0)
            return sum(moved.histogram()[255:])
    except Exception as exc:                # noqa: BLE001
        note(f"could not compare the two shots ({exc})")
        return None


def focus_app_group(s: Session, app_id: str,
                    prefer: dict | None = None) -> bool:
    """Scroll the strip to the group holding `app_id`, and say whether it
    worked.

    `focus_leftmost_group` frames a strip; this picks a *subject*. The
    last shot of the film is about one window — the document two other
    participants have been writing into — and an even share of the strip
    with the file manager scrolled up in front of it is a shot of the
    wrong thing. It has been exactly that: a payoff frame showing an
    empty folder listing, under a caption about a document.

    Aimed at the tab pill for the same reason as `focus_leftmost_group`:
    a click in the body reaches the client and selects whatever is under
    it.
    """
    laid = painted_tiles(s)
    tiles = [t for sid, t in sorted(laid.items()) if t["app_id"] == app_id]
    if not tiles:
        note(f"{s.name}: nothing on screen is {app_id}, so the strip is "
             f"left where it is")
        return False
    # By the end of the film two windows on this screen announce the same
    # app id — the session's own, and the peer's, replicated in. They are
    # showing the same document, so either would do; `prefer` names the
    # one the beat has been talking about, so the shot is of the window
    # the audience watched arrive.
    want = (prefer or {}).get("surface_id")
    chosen = next((laid[int(want)] for _ in (0,)
                   if want is not None and int(want) in laid
                   and laid[int(want)]["app_id"] == app_id), tiles[0])
    x, y, w, _h = chosen["rect"]
    py = y - 18.0
    if py < TOPBAR_H:
        # One window filling the strip wears no pill, and 18px above it is
        # the workspace switcher. Already the only thing on screen, so
        # there is nothing to scroll to anyway.
        return True
    s.click_at(max(4.0, x + min(90.0, w / 2.0)), py)
    s.settle(0.8)
    note(f"{s.name}: brought {app_id} up in the strip for the shot")
    return True


def peer_sync_card_up(s: Session) -> bool:
    """Whether the peer-sync card is being painted over the middle of the
    screen.

    All three fields, because the compositor paints the card whenever any
    one of them is set (`overlay_host/mod.rs`: the draw returns early only
    when `input`, `token` *and* `invite_name` are all `None`). Checking
    two of them made an open invite composer read as no card at all, so
    `dismiss_peer_sync_card` returned without dismissing it and the
    question "who are you inviting?" stayed on screen over the beat after
    it.
    """
    ps = s.chrome()["peer_sync"]
    return (ps["invite_token"] is not None
            or ps["connect_input"] is not None
            or ps.get("invite_name") is not None)


def dismiss_peer_sync_card(s: Session, what: str) -> None:
    """Close the peer-sync card, and prove it closed.

    Sending Escape and moving on is what put a stale modal over the two
    most interesting beats in a recorded run: the card sat in the middle
    of the frame for a minute after the invite, and the word processor
    opened behind it. Escape reaches the overlay host only when nothing
    else has taken the key first, so the dismissal is retried and then
    asserted — a beat filmed behind a dialog is a beat nobody can read.

    It also holds a bearer credential. The token is a signed blob that
    admits its holder to a real room, and it stays legible on screen for
    as long as the card is up, so the frame is cleared as soon as the
    token has been captured rather than whenever the next beat happens
    to press a key.
    """
    for _ in range(4):
        if not peer_sync_card_up(s):
            return
        s.key("Escape")
        s.settle(0.4)
    claim(f"{s.name}'s {what} card is off the screen",
          peer_sync_card_up(s), False)


def show_collaborators(s: Session) -> str:
    """Open the room's people from the affordance on screen.

    The top bar's avatar stack is the affordance (`topbar_chrome.rs`
    toggles `overlays.collaborators`), and it is the one a person reaches
    for. It used to be *there* only once somebody was in the room: an
    empty roster published `Rect::ZERO`, so at the one moment a host
    wants to invite the *first* person there was nothing to click, and
    the panel could only be reached by its address — which is not a
    gesture and is not what a viewer is being shown.

    It keeps its geometry now and reads `Invite`, so this is a click on
    every run and the address below is a diagnosis rather than a route:
    taking it is a GAP, because a beat whose subject is the chrome has
    then not shown the chrome. The route is returned and printed either
    way.
    """
    if s.named_rect("topbar_avatar_stack"):
        s.click_named("topbar_avatar_stack")
        s.settle(1.0)
        return "the avatar stack in the top bar"
    gap(f"{s.name}: the top bar published no avatar stack to click, so the "
        f"room's people had to be opened by address rather than by the "
        f"gesture this beat is about")
    resp = s.invoke_action(AGENT_PROVIDER, "collaborators.open")
    if "err" in resp:
        note(f"{s.name}: collaborators.open refused — "
             f"{json.dumps(resp['err'])[:120]}")
        return "nothing - the panel could not be opened"
    s.settle(1.0)
    return "its address (the avatar stack was not on screen)"


def close_collaborators(s: Session) -> None:
    if s.overlays().get("collaborators"):
        s.key("Escape")
        s.settle(0.4)


def invite_from_the_panel(host: Session, name: str, shots: str | None = None,
                          tag: str | None = None) -> str | None:
    """Invite somebody from the panel a person opened, with the mouse.

    Assumes the collaborators panel is already up — clicking `Invite`
    closes it, because the card carries the question and then the token
    and a menu over that is a menu over the thing the click was for.

    Every step is a click on a painted control, so a control that is not
    drawn, not hit-testable or not wired fails here instead of being
    routed around: `collab_invite` opens a composer that asks *who*,
    `peer_sync_create` mints against that answer and puts the token on
    the system clipboard, and the card says so where the host is looking.
    Raising the same card by its address would have proved only the last
    of those.

    The only thing typed is a person's name. It is what the room will
    call whoever redeems this token, it is held on this machine until
    they do, and it never crosses the wire — a guest who could announce
    its own name could announce anybody's.
    """
    invite = host.named_rect("collab_invite")
    claim("the panel offers a way to invite somebody", invite is not None, True)
    if invite is None:
        gap("the collaborators panel painted no Invite button, so the "
            "invite cannot be raised from the panel a person opened")
        return None
    note(f"Invite at {[round(v) for v in invite]}, Join beside it at "
         f"{[round(v) for v in (host.named_rect('collab_join') or [])]}")

    step("he presses Invite")
    clicked = host.click_named("collab_invite")
    if "err" in clicked:
        gap("the panel's Invite button would not take a click - "
            f"{json.dumps(clicked['err'])[:120]}")
        return None
    host.settle(0.8)

    step("and it asks who, before it mints anything")
    # The whole reason the composer exists. A button that minted an
    # anonymous token left the host's own roster reading
    # `peer@12191268537764683811` once the guest arrived, because nothing
    # in the flow had ever been told a name — and a name is governance
    # here, so it cannot be supplied by the person arriving.
    asked = wait_for(
        lambda: host.chrome()["peer_sync"]["invite_name"] is not None,
        timeout=15)
    claim("the card asks who the invitation is for", bool(asked), True)
    if not asked:
        gap("Invite opened no composer, so there was nobody to name and "
            "nothing to mint against")
        dismiss_peer_sync_card(host, "invite")
        return None
    claim("and nothing was minted before it was answered",
          host.chrome()["peer_sync"]["invite_token"], None)
    host.type_text(name)
    host.settle(0.5)
    claim(f"the field took the name {name!r}",
          host.chrome()["peer_sync"]["invite_name"], name)
    # Who it is for, on screen, before anything has been minted. One of
    # the four frames this beat exists to let somebody read.
    read_hold()
    if shots and tag:
        host.snapshot(os.path.join(shots, f"{tag}-naming.png"))

    step("and presses Create invite")
    created = host.click_named("peer_sync_create")
    if "err" in created:
        gap("the composer's Create invite button would not take a click - "
            f"{json.dumps(created['err'])[:120]}")
        dismiss_peer_sync_card(host, "invite")
        return None
    token = wait_for(lambda: host.chrome()["peer_sync"]["invite_token"],
                     timeout=30)
    claim("a token was minted", bool(token), True)
    if not token:
        dismiss_peer_sync_card(host, "invite")
        return None
    claim(f"and the card says whose it is — {name!r}",
          host.chrome()["peer_sync"]["invite_for"], name)
    status = host.chrome()["peer_sync"]["status"] or ""
    claim("and that it was copied, where he is looking",
          "copied" in status.lower(), True)
    note(f"the card reads {status!r}")
    # What leaves here is what the button produced, not a value the
    # harness already had in a variable: a copy that silently truncated
    # fails the join on the other machine rather than being papered over.
    copied = wait_for(lambda: host.clipboard_read(), timeout=10)
    claim("the token really is on his system clipboard, put there by the "
          "button", copied == token, True)
    if copied != token:
        note(f"the card shows {len(token)} characters; the clipboard holds "
             f"{len(copied or '')}")
    # The card, whole: whose it is, the short code that reads the same on
    # both screens, and that it is on the clipboard. This is the frame
    # the invitation has flashed past in every previous cut, so it is
    # held for longer than anything else in the film.
    read_hold(READ_HOLD_S * 2)
    if shots and tag:
        host.snapshot(os.path.join(shots, f"{tag}-copied.png"))
    # The moment it has been read, and not one beat later: it is a modal
    # over the middle of the screen and it is showing a live token.
    dismiss_peer_sync_card(host, "invite")
    return copied or token


def join_room(guest: Session, token: str, via: str,
              shots: str | None = None, tag: str | None = None) -> bool:
    """Put `guest` into the room `token` names, by pasting and pressing Join.

    The token is never typed. It is put on this session's clipboard —
    that is the hop between two machines, and it belongs to a person: a
    chat message, an email, a USB stick, and not the OS's business — and
    from there the card's own **Paste** button lifts it into the prompt
    and its **Join** button redeems it. Both are the gestures a person
    makes, and both are load-bearing: if the paste does nothing the
    prompt stays empty and this fails, rather than quietly falling back
    to typing a string the harness already had. Nobody types three
    hundred characters of base64 correctly, twice, on a stage.

    `via` says how the prompt is raised, and the two routes are different
    claims about the same card:

      `panel`    the top bar, then **Join** in the collaborators panel.
                 The gesture a person makes, and it fails here when
                 either control is unpainted or unwired.
      `action`   the prompt's stable address, `peer.connect`. What a
                 participant with no screen has instead of a mouse.
    """
    guest.clipboard_write(token)
    if via == "panel":
        step("he opens the room's people on his own machine")
        opened = show_collaborators(guest)
        note(f"{guest.name} opened it from {opened}")
        claim(f"{guest.name}'s collaborators panel is up",
              guest.overlays()["collaborators"], True)
        if shots and tag:
            guest.snapshot(os.path.join(shots, f"{tag}-panel.png"))
        read_hold()

        step("and presses Join")
        seat = guest.named_rect("collab_join")
        claim("the panel offers a way into somebody else's room",
              seat is not None, True)
        if seat is None:
            gap("the collaborators panel painted no Join button, so a room "
                "cannot be entered from the panel a person opened")
            return False
        clicked = guest.click_named("collab_join")
        if "err" in clicked:
            gap("the panel's Join button would not take a click - "
                f"{json.dumps(clicked['err'])[:120]}")
            return False
        guest.settle(0.8)
    elif via == "action":
        resp = guest.invoke_action(AGENT_PROVIDER, "peer.connect")
        if "err" in resp:
            raise RuntimeError(f"{guest.name}: peer.connect refused: {resp['err']}")
    else:
        raise ValueError(f"unknown join route {via!r}")

    if not wait_for(lambda: guest.chrome()["peer_sync"]["connect_input"] is not None,
                    timeout=15):
        gap(f"{guest.name}: nothing opened a connect prompt, so there was "
            f"nowhere to put the token")
        return False
    read_hold()

    step("he presses Paste")
    pasted_click = guest.click_named("peer_sync_paste")
    if "err" in pasted_click:
        gap("the prompt's Paste button was not addressable - "
            f"{json.dumps(pasted_click['err'])[:120]}")
        dismiss_peer_sync_card(guest, "connect")
        return False
    filled = wait_for(
        lambda: guest.chrome()["peer_sync"]["connect_input"] or None, timeout=10)
    claim("the prompt filled itself from the clipboard, unretyped",
          filled == token, True)
    if filled != token:
        note(f"the prompt holds {len(filled or '')} characters; the token is "
             f"{len(token)}")
    if shots and tag:
        guest.snapshot(os.path.join(shots, f"{tag}-pasted.png"))
    # The same short code, on the other machine, beside the words telling
    # you to check it matches. Held, for the same reason as above.
    read_hold(READ_HOLD_S * 2)

    step("and Join")
    joined_click = guest.click_named("peer_sync_join")
    if "err" in joined_click:
        gap("the prompt's Join button was not addressable - "
            f"{json.dumps(joined_click['err'])[:120]}")
        dismiss_peer_sync_card(guest, "connect")
        return False
    joined = wait_for(
        lambda: (guest.chrome()["peer_sync"]["status"] or "").startswith("Joined"),
        timeout=15)
    if shots and tag:
        guest.snapshot(os.path.join(shots, f"{tag}-joined.png"))
    read_hold()
    dismiss_peer_sync_card(guest, "connect")
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
    speaks (`crates/sdk/ui-core/src/collaboration.rs`), announces the same
    `(app_id, cluster_id, record)` address the word processor announces,
    and then sends the one frame every toolkit answers with its whole
    document: `Presence { Arrived }` (`collab_broker.rs:1149`, and the
    same handshake `tilo-agent` opens with, `apps/agent/src/session.rs:72`).

    What comes back is therefore **produced by the app running in that
    session, out of the document that app is holding**. It is not the
    wire being read back to itself: a snapshot that carries the text
    means the far app's own document carries it, and a snapshot that
    does not means the two documents have not converged. Both are
    reported; neither is inferred from the other.
    """

    # The presence scope the word processor's toolkit answers on. Same
    # constant `tilo-agent` uses, for the same reason: a different scope
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
        seen: set[int] = set()          # blobs already searched, by id
        while time.time() < deadline:
            docs = self.documents()
            if docs:
                newest = docs[-1]
            # Every frame, not just the newest one. More than one
            # participant answers `Arrived` with a full document on this
            # address — the agent does too — so the newest frame is not
            # necessarily the app's, and reading only that one reports
            # another participant's document as the app's empty page.
            #
            # A delta carries the text too, and does when the far app was
            # already attached while it was typed. Both kinds are worth
            # searching; only a document is worth reporting a size for.
            for blob in docs + self.deltas():
                key = hash(blob)
                if key in seen:
                    continue
                seen.add(key)
                if _carries(blob, needle):
                    return blob, True
            time.sleep(0.5)
        return newest, False


def _carries(blob: bytes, needle: str) -> bool:
    """Whether an Automerge blob holds `needle` as text.

    Not a byte search. Automerge DEFLATE-compresses each column of a
    change chunk once it is over a few hundred bytes, so a phrase a
    person typed is genuinely in the document and genuinely absent from
    its bytes — and a harness that grepped the bytes reported an empty
    page for a document that had the sentence in it. Worse, it failed
    the same way whatever went wrong, so a correct document read as a
    sync failure and sent people looking at the network.

    A per-keystroke delta cannot match either: it carries one character,
    and no single character is a 33-character phrase. That is a real
    absence rather than a misreading, and the document frames are where
    the answer is.

    Decoding is delegated to `tilo-doc-text`, which walks the object
    graph, so this needs no knowledge of the app's schema. Without that
    binary there is no honest answer available, and saying so beats
    guessing from bytes.
    """
    if not DOC_TEXT:
        return False
    try:
        done = subprocess.run([DOC_TEXT, "-", "--contains", needle],
                              input=blob, stdout=subprocess.DEVNULL,
                              stderr=subprocess.DEVNULL, timeout=20)
    except (OSError, subprocess.SubprocessError):
        return False
    return done.returncode == 0


def carries_bytes(blob: bytes, needle: bytes) -> bool:
    """Whether an Automerge save contains `needle` verbatim.

    The counterpart to [`_carries`] for something that is not text. The
    word processor stores an image as `ScalarValue::Bytes` under its
    BLAKE3 hash (`qdoc/src/model.rs:2005`), so the PNG's own bytes are
    in the document — which is what makes this answerable without an
    Automerge binding.

    Columns over a size threshold are DEFLATE'd, so a plain `in` is not
    enough: every plausible zlib header is inflated and searched too.
    Bounded at the first megabyte, because the scan is quadratic in the
    worst case and a document this big is already the answer.
    """
    if needle in blob:
        return True
    for start in range(min(len(blob), 1 << 20)):
        if blob[start] != 0x78 or start + 1 >= len(blob):
            continue
        if blob[start + 1] not in (0x01, 0x5E, 0x9C, 0xDA):
            continue
        try:
            if needle in zlib.decompressobj().decompress(blob[start:], 1 << 25):
                return True
        except zlib.error:
            continue
    return False


def image_is_in_the_document(session: Session, room: str, png: bytes,
                             timeout: float = 20.0) -> tuple[bool, int]:
    """Ask the running word processor for its document, and look for the
    image in it.

    A late joiner on this session's broker announces the app's address
    and sends the one frame every toolkit answers with its whole
    document. What comes back was produced by the app out of the
    document it holds — so this is the difference between "an image
    appeared on screen" and "the image is in the document", which is the
    claim that matters, because it is the one that replicates.

    The needle is the PNG's signature plus its IHDR: enough to be *this*
    file rather than any file, short enough to survive being stored
    across an Automerge column boundary.

    Deliberately run after the camera has stopped: attaching announces a
    presence, and a phantom collaborator arriving mid-take would be a
    thing on screen that the demo did not do.
    """
    try:
        reader = DocumentReader(session.collab_json_sock, room, record=None)
    except OSError:
        return False, 0
    try:
        deadline = time.time() + timeout
        newest = 0
        needle = png[:24]
        while time.time() < deadline:
            for blob in reader.documents() + reader.deltas():
                newest = max(newest, len(blob))
                if carries_bytes(blob, needle):
                    return True, len(blob)
            time.sleep(0.5)
        return False, newest
    finally:
        reader.close()


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


def a_real_png(width: int = 512, height: int = 320) -> bytes:
    """A photograph — a sun over ridged hills, drawn pixel by pixel.

    It has to read as a photograph at two sizes at once: as a 16px
    thumbnail in a file row, where only the sky/land split and the sun
    survive, and as a picture in the document, where a flat gradient
    would be obviously synthetic. So there is a horizon, a light source,
    receding ridges that get paler with distance, and a little grain.

    Generated rather than embedded because a checked-in JPEG is a binary
    blob nobody can review, and because the demo wants a payload whose
    size it chose: the whole point of beat 8 is that ~98 KB of image
    crosses into the document where a keystroke moves ~100 bytes, and
    that number should come from this function rather than from whatever
    file somebody happened to drop in the tree.
    """
    import math
    import struct
    import zlib

    horizon = int(height * 0.56)
    sun_x, sun_y = int(width * 0.72), int(horizon * 0.42)
    sun_r = max(6.0, height * 0.055)

    # Each ridge: how far below the horizon its crest sits, its
    # amplitude, its wavelength, a phase, and how dark it is. Front
    # ridges are darker and rougher — that is what makes them read as
    # nearer rather than merely lower.
    # The `base` of each has to clear its own amplitude, or the crest
    # rises above the horizon, gets clipped by the sky, and paints as a
    # dead flat plate with a notch in it — which is precisely what the
    # first version of this drew.
    ridges = [
        (0.045, 0.055, 1.7, 0.4, (96, 122, 132)),
        (0.130, 0.075, 1.1, 2.1, (66, 94, 100)),
        (0.270, 0.095, 0.8, 4.3, (42, 68, 70)),
        (0.470, 0.130, 0.6, 1.2, (24, 44, 44)),
    ]

    def clamp(v: float) -> int:
        return 0 if v < 0 else (255 if v > 255 else int(v))

    # Deterministic grain. `random` would make the file — and so the
    # document delta — differ between runs, and the demo prints that
    # number.
    def grain(x: int, y: int) -> float:
        h = (x * 374761393 + y * 668265263) & 0xFFFFFFFF
        h = (h ^ (h >> 13)) * 1274126177 & 0xFFFFFFFF
        return ((h >> 16) & 0xFF) / 255.0 - 0.5

    rows = bytearray()
    for y in range(height):
        rows.append(0)  # filter: none
        for x in range(width):
            if y < horizon:
                # Sky: deep above, warm and pale toward the horizon.
                t = y / max(1, horizon)
                r, g, b = (58 + 150 * t, 108 + 122 * t, 165 + 70 * t)
                # The sun, and the haze around it.
                d = math.hypot(x - sun_x, y - sun_y)
                if d < sun_r:
                    r, g, b = 255, 250, 214
                elif d < sun_r * 7.0:
                    k = (1.0 - (d - sun_r) / (sun_r * 6.0)) ** 2.4
                    r, g, b = (r + (255 - r) * k, g + (250 - g) * k,
                               b + (214 - b) * k)
            else:
                # Land, painted back to front so a nearer ridge covers
                # the one behind it. What shows through above the
                # furthest crest is haze, not ground — filling this with
                # the ground colour put a dark band along the horizon,
                # and filling it with one flat haze colour put a flat
                # one there instead. So it fades out of the sky.
                t = min(1.0, (y - horizon) / max(1.0, height * 0.10))
                r, g, b = (208 - 76 * t, 230 - 78 * t, 235 - 77 * t)
                for base, amp, freq, phase, colour in ridges:
                    crest = (horizon + height * base
                             + math.sin(x / width * math.pi * 2 * freq + phase)
                             * height * amp * 0.5
                             + math.sin(x / width * math.pi * 9 * freq + phase * 3)
                             * height * amp * 0.12)
                    if y >= crest:
                        # A little vertical shading inside each band, so
                        # it is not a flat cut-out.
                        k = min(1.0, (y - crest) / (height * 0.30))
                        r, g, b = (colour[0] * (1 - 0.35 * k),
                                   colour[1] * (1 - 0.35 * k),
                                   colour[2] * (1 - 0.35 * k))
            # Enough grain to break up the banding a pure gradient shows
            # on a large flat sky, and no more: grain is nearly
            # incompressible, and this file's size *is* the payload the
            # demo measures crossing into the document. At 7.0 the same
            # picture was 294 KB.
            n = grain(x, y) * 2.5
            rows += bytes((clamp(r + n), clamp(g + n), clamp(b + n)))

    def chunk(tag: bytes, payload: bytes) -> bytes:
        return (struct.pack(">I", len(payload)) + tag + payload
                + struct.pack(">I", zlib.crc32(tag + payload) & 0xFFFFFFFF))

    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(bytes(rows), 9))
            + chunk(b"IEND", b""))


def stage_home(path: str) -> str:
    """A home of this session's own, staged to look like one.

    Two things are on screen here that are easy to get wrong and
    impossible to unsee.

    **The path.** Files draws the directory it is showing, component by
    component, and a home under the checkout renders as
    `… › tilo › .claude › worktrees › yc-demo › target › …`. A
    viewer reads that as a harness pointed at a scratch directory, and
    `.claude` tells them which tooling built it. So the homes live under
    [`HOMES_ROOT`], whose components each read as a machine's own.

    **The contents.** A home holding one folder called `Demo` is not a
    home anybody believes, and it gives a drag beat nothing to pick up.
    The tree below is the ordinary furniture of a home directory — the
    XDG folders the sidebar already lists, a couple of documents, and
    the photograph beat 8 drags in.

    Staging a *file tree* is not the same as fabricating content: there
    is no invented correspondence here and nobody's name on anything.
    The documents carry the few lines the demo itself writes and reads.
    """
    for folder in ("Documents", "Downloads", "Pictures", "Projects"):
        os.makedirs(os.path.join(path, folder), exist_ok=True)
    # The photograph beat 8 drags into the document, written twice.
    #
    # In `Pictures`, because that is where it belongs and where a person
    # would look for it — and at the top of the home, because that is
    # the directory Files is *showing* when the beat starts, and a drag
    # can only pick up a row that is on screen. Navigating there first
    # would be two more client-aimed clicks before the gesture the beat
    # is actually about, each of which can miss.
    #
    # Both are the same bytes, so whichever is dragged produces the same
    # payload and the same document delta.
    photo = a_real_png()
    for where in (os.path.join(path, "Pictures", PHOTO_NAME),
                  os.path.join(path, PHOTO_NAME)):
        with open(where, "wb") as fh:
            fh.write(photo)
    with open(os.path.join(path, "Documents", "Launch plan.md"), "w") as fh:
        fh.write("# Launch plan\n\n- Venue - The Hall\n- Date - 3 Sep\n")
    with open(os.path.join(path, "Documents", "Checklist.md"), "w") as fh:
        fh.write("# Checklist\n\n- Doors\n- Sound\n- Seating\n")
    with open(os.path.join(path, "Projects", "README.md"), "w") as fh:
        fh.write("# Projects\n")
    return path


class Registry:
    """A signed registry on loopback, serving one real `.tapp`.

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


def type_into_the_document(s: Session, wp: dict | None, text: str) -> bool:
    """Put the caret in the page and type, the way a person would.

    The word processor is a foreign client, so its insides are not in
    the compositor's widget registry and there is no name to aim at.
    Its *tile* has one, which is as close as the shell can get: click
    into the middle of the page, then type.
    """
    surface = (wp or {}).get("surface_id")
    stem = f"foreign-surface-{surface}"
    named = next((n for n in s.target_names()
                  if n == stem or n.startswith(stem + "[")), None)
    rect = s.named_rect(named) if named else None
    if not rect:
        others = [n for n in s.target_names()
                  if n.startswith("foreign-surface-")]
        note(f"{s.name}: no laid-out tile for surface {surface}; "
             f"laid out now: {others or 'none'}")
        return False
    bx, by, bw, bh = rect
    s.click_at(bx + bw * 0.4, by + bh * 0.35)
    s.settle(1.0)
    # To the end of what is already there, rather than wherever the
    # click happened to land. By this point the document has a sentence
    # and an image in it, and a caret dropped into the middle of that
    # writes the list into the middle of it — or replaces a selected
    # image with the first character typed.
    #
    # A *held* Control, not the `modifiers` mask. That mask is the
    # compositor's own, read by chrome; a client resolves keycodes
    # through the seat's xkb state, which the mask never touches. So the
    # `modifiers(ctrl=True)` this used to do sent a bare `End` — end of
    # the *line*, not of the document — and did it silently, which is
    # exactly the kind of thing that only shows up as a list typed into
    # the middle of a paragraph.
    s.key_down("Control")
    s.key("End")
    s.key_up("Control")
    s.settle(0.5)
    s.type_prose(text)
    s.settle(1.0)
    return True


def agent_binary() -> str | None:
    """`tilo-agent`, if it has been built.

    A separate process rather than a compositor: the whole point of it
    is that a participant can hold a document, a caret and a name in the
    room's history without holding a screen. Absent is a supported
    state — the beat says so and stops, rather than substituting
    something that looks similar.
    """
    for profile in ("demo", "release", "debug"):
        path = os.path.join(REPO, "target", profile, "tilo-agent")
        if os.path.exists(path):
            return path
    return None


def run_the_agent(agent: Session, room: str, shots: str,
                  timeout: float = 120.0) -> bool | None:
    """Let the agent join the document and carry out one instruction.

    It connects to its own session's broker, which is how its edits
    reach the other two: the same Iroh publish path carries every peer's edits
    take. `--no-model` keeps the run reproducible and off the network —
    the vocabulary is a keyword table, and an instruction must never
    wait on a request.

    Returns None when the binary is not built, so a caller can say that
    rather than report a failure it did not observe.
    """
    binary = agent_binary()
    if binary is None:
        return None
    log_path = os.path.join(shots, "agent.log")
    with open(log_path, "w") as log:
        # `--app` is named rather than left to its default, because the
        # default is the whole reason this beat once filmed a file
        # manager: with no address to narrow on, the broker's fan-out is
        # room-wide, the agent is offered every document open in the room
        # and takes whichever arrives first — and in one run that was the
        # file explorer's, whose actor id (`files-2`) then refused to
        # merge with the word processor's when that finally turned up.
        #
        # `--record` is the other half of that address and is deliberately
        # *not* passed: it names the file a document was opened from, and
        # this document has no file. It is the untitled document the app
        # created when beat 5 launched it, so its record is empty, and
        # `--record ""` would be a different address from "no record"
        # rather than the same one. Staging a file to open instead would
        # give this something to name — it needs a way to launch a
        # packaged app *on* a path, which the applications action does
        # not expose today.
        proc = subprocess.Popen(
            [binary, "--workspace", str(room), "--name", "Claude",
             "--app", WORD_PROCESSOR,
             "--socket", agent.collab_json_sock, "--no-model", "--once",
             "--pace-ms", "250"],
            stdout=log, stderr=subprocess.STDOUT, cwd=REPO,
            preexec_fn=die_with_parent(enabled=not KEEP_SESSIONS))
        if not KEEP_SESSIONS:
            track(proc)
        try:
            code = proc.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            proc.kill()
            note(f"the agent did not finish within {timeout:.0f}s; "
                 f"see {log_path}")
            # A bare timeout sends the next person to read a hung
            # process. The usual cause is upstream of the agent and is
            # visible in the session's own log: the broker computes an
            # addressed channel key and then fans out room-wide anyway,
            # so every app in the room answers the agent's handshake and
            # it is offered a document from an app it did not ask for.
            # Said here so it is diagnosed once rather than every time.
            try:
                with open(agent.log_path) as fh:
                    session = fh.read()
            except OSError:
                session = ""
            wide = [ln for ln in session.splitlines()
                    if "room_wide=true" in ln and "addressed=" in ln]
            others = [ln for ln in session.splitlines()
                      if "recv Arrived" in ln
                      and WORD_PROCESSOR not in ln.split("]")[0]]
            if wide or others:
                note("the broker fanned this session's collaboration "
                     "traffic room-wide while carrying an addressed "
                     "channel key, so apps the agent did not ask for "
                     "answered its handshake:")
                for ln in (wide[-1:] + others[-2:]):
                    note(f"    {ln.strip()[:200]}")
                note("that is a broker fix, not an agent or a demo one — "
                     f"see {agent.log_path}")
            return False
    if code != 0:
        with open(log_path) as fh:
            tail = fh.read()[-400:]
        note(f"tilo-agent exited {code}: {tail}")
    return code == 0


# --------------------------------------------------------------------
# the demonstration
# --------------------------------------------------------------------

# Set by `main`, so a beat can report on all three sessions without
# every beat function taking all three.
SESSIONS: list[Session] = []

# The headless viewport every session was started at, as (w, h). Set by
# `main` from the same string the compositors were given, so a beat can
# ask whether what it is about to claim is actually inside the frame.
VIEWPORT: tuple[int, int] = (0, 0)


def beat_one(ada: Session, shots: str) -> tuple[dict, dict | None]:
    banner("1.  an empty desktop, and Files opened on it")
    # The host's screen. A first boot really is empty here — one
    # workspace, no strips, no windows — so "empty" is read off the tree
    # rather than asserted about a backdrop.
    cut("ada")

    ids = {s.name: peer_id_of(s) for s in SESSIONS}
    for s in SESSIONS:
        note(f"{s.name:8} pid {s.proc.pid:<8} user {s.user:<8} peer {ids[s.name]}")
    claim("three separate processes", len({s.proc.pid for s in SESSIONS}), 3)
    claim("three separate Iroh identities", len(set(ids.values())), 3)

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
    # The surface travels with it: beat 8 drags a file *out of this
    # window*, and by then a second client is on screen announcing the
    # same wayland app-id. The surface the launch saw paint is the only
    # thing that tells the two apart.
    return host_ws, painted


def beat_two(ada: Session, bob: Session, host_ws: dict,
             shots: str) -> None:
    banner("2.  ada invites bob")
    cut("ada")

    step("he opens the room's people")
    # Through the chrome, at a pace a viewer can follow, and every step of
    # it a click on something painted. This whole beat used to pass in
    # under a second because it was invoked as an action: every claim held
    # and the collaboration UI never appeared, which is the one thing the
    # beat exists to put on screen.
    via = show_collaborators(ada)
    note(f"opened from {via}")
    claim("the collaborators panel is up", ada.overlays()["collaborators"],
          True)
    here = ada.active_workspace().get("collaborators") or []
    note(f"the room holds {len(here)} collaborator(s) — an empty roster is "
         f"exactly when the way in matters most, and it is the case that "
         f"used to paint nothing at all")
    read_hold()
    ada.snapshot(os.path.join(shots, "02a-collaborators-empty.png"))

    token = invite_from_the_panel(ada, bob.user, shots, "02b")
    claim("ada raised an invite from the panel he opened", bool(token),
          True)
    if not token:
        raise SystemExit("no invite token: the rest cannot run")
    note("the token is a signed blob carrying the room id, the owner's "
         "address and an expiry. The name is not in it: it waits on this "
         "machine until somebody redeems the token and is written into the "
         "room document then, by the room's own authority, so both "
         "machines can attribute a cursor to a person nobody self-declared")

    # Redeeming is bob's, and it is his screen that changes — and the
    # film does not go there for it.
    #
    # This used to be a cut, and it is the most tempting one in the whole
    # storyboard: it is a real gesture on a second machine, and every
    # step of it is proven below. What it shows is a person pasting a
    # string into a dialog, which is the least surprising thing in this
    # system, and it costs the audience a cut out and a cut back at the
    # point where they are still working out who the first person is.
    #
    # What they see instead is ada's roster gaining a member two
    # beats later, on the screen they are already watching. Every claim
    # here still holds; the pictures are written to `shots/` and are not
    # in the cut.
    step("the token travels — by hand, off ada's clipboard")
    with off_camera("bob redeeming the invitation on his own machine — "
                    "ada's roster gains him on screen shortly"):
        claim("bob joined",
              join_room(bob, token, via="panel", shots=shots, tag="02c"), True)
        claim("bob's own state holds ada's room",
              bob.active_workspace()["room"], host_ws["room"])
        claim("and his own copy names ada as owner",
              bob.active_workspace()["owner"], ada.user)


def beat_three(ada: Session, agent: Session, host_ws: dict,
               shots: str) -> None:
    banner("3.  ada invites the agent")
    # No cut anywhere in this beat. It runs off camera (see `run`): it is
    # the same card the film has just spent fifteen seconds on, made out
    # to a different name, and the half of it that is actually new — an
    # agent is a subject in the permission model, not a peer — is a
    # question asked of the authority gate and answered in this
    # transcript. There is nothing in it that a second look at the same
    # modal would show.
    step("he opens the room's people again — bob is in it now")
    via = show_collaborators(ada)
    note(f"opened from {via}")
    claim("the collaborators panel is up", ada.overlays()["collaborators"],
          True)
    ada.snapshot(os.path.join(shots, "03a-collaborators-bob.png"))

    step("a second invitation — but an agent is not a peer")
    token = invite_from_the_panel(ada, agent.user, shots, "03b")
    claim("ada raised a second invite from the same panel", bool(token),
          True)
    if not token:
        raise SystemExit("no second token: the agent cannot join")

    # No cut. The agent has no screen worth filming and showing one
    # inverts the claim: it is a participant in somebody else's room, so
    # it appears as a row in a roster, a caret in a document and a name
    # in a history — never as a desktop of its own.
    claim("the agent joined the room", join_room(agent, token, via="action"),
          True)
    claim("the agent's own state holds ada's room",
          agent.active_workspace()["room"], host_ws["room"])
    # Where the two routes differ, said plainly. The agent has no mouse
    # to reach a top bar with, so it raises the same card by its stable
    # address — `peer.connect`, one of the actions beat 9 enumerates —
    # and redeems with the card's own Paste and Join. The token is not
    # typed on either machine, and there is no route here that types it.
    note("the agent reached the same prompt by address rather than by "
         "opening a panel; the token was pasted and redeemed with the "
         "card's own buttons, never typed")

    step("what the agent is allowed to do, before ada says so")
    # The more interesting half, and the one that makes an agent a
    # different kind of participant: it is a subject in the permission
    # model, `agent:<id>`, and its authority is a grant on a scope. Asked
    # of the production gate, which answers about a verb on a scope.
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
          may_write("so.tilo.terminal"), False)
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

    cut("ada")

    step("read ada's room back out of the other two")
    # Neither of these is ada's own state — each guest independently
    # reports the room it now holds — and neither is filmed from the
    # guest. Asking a far session what it holds is a question, and a
    # question has no picture: what it produces is a line in this
    # transcript. The camera stays where the story is, and what bob
    # holds is *seen* in the last shot of the film rather than described
    # over a screenshot of his backdrop.
    claim("bob's own copy carries ada's name for the room",
          bob.active_workspace()["name"], host_ws["name"])
    # Read from the agent's session, shot on nobody's: it is asserted
    # here and *seen* in the roster on ada's screen below.
    claim("the agent's own copy carries it too",
          agent.active_workspace()["name"], host_ws["name"])

    step("and the room's people, from the top bar")
    # Now there is a roster, so the affordance exists: the avatar stack
    # is drawn and clicking it opens the panel. This is also where the
    # names have to hold up — a roster listing `peer-2` and
    # `peer@11895642303397647149` beside `bob` is three label styles
    # for three participants, and a room nobody can read.
    via = show_collaborators(ada)
    note(f"opened from {via}")
    claim("the collaborators panel is up on ada's screen",
          ada.overlays()["collaborators"], True)
    # Who is in this room, read off the room's own capability list —
    # which *is* the membership (`state.rs:877`: "Room membership lives
    # here"), and is the same list the panel on screen is drawn from.
    #
    # Read here rather than from `peers`, which carries pointers and
    # nothing else: a `peers()["roster"]` lookup returned an empty list
    # on every run this demo has ever made, and printed nothing, so the
    # roster was never actually being checked.
    #
    # This is also the honest place to look for the agent. Claude is a
    # participant in this room and has no mouse — a pointer that has
    # never moved publishes nothing, by design — so asserting a cursor
    # for it would be asserting something that should not exist. What
    # makes it a participant is that it holds a capability in the room,
    # and that is what is checked.
    caps = ada.active_workspace().get("capabilities") or []
    members: dict[str, list[str]] = {}
    for cap in caps:
        subject = json.dumps(cap.get("subject"))
        for name in (ada.user, bob.user, agent.user):
            if f'"{name}"' in subject or f":{name}" in subject:
                members.setdefault(name, []).append(str(cap.get("verb")))
    if not caps:
        # Said as a gap rather than asserted away. An empty list here is
        # this reader looking in the wrong place, not a room with nobody
        # in it — three participants joined it two beats ago.
        gap("the active workspace reported no capability list, so who is "
            "in this room could not be read off it")
    else:
        note(f"the room's own capability list names {sorted(members)} "
             f"across {len(caps)} capabilities")
        claim("the agent is in the room the same way the people are — by "
              "holding a capability in it", agent.user in members, True)
        if agent.user in members:
            note(f"{agent.user} holds {sorted(set(members[agent.user]))} "
                 f"here — which is what makes it a member of this room: a "
                 f"capability, granted by the owner, in the same list the "
                 f"people are in")
    read_hold()
    ada.snapshot(os.path.join(shots, "03a-roster.png"))
    close_collaborators(ada)

    step("and the cursors — on a desktop with nothing open on it")
    # `peers` is read from the same PresenceCtx the overlay draws from,
    # which is the only way to tell "no peer arrived" from "a peer
    # arrived and was not drawn" — those make the same picture.
    #
    # This beat is deliberately shot on an empty desktop, and that used
    # to be exactly the case that could not work: a session only put its
    # cursor on the wire as a side effect of some local app publishing
    # one, so two people looking at empty desktops published nothing
    # between them. It is level-triggered from the frame loop now, and a
    # pointer that has stopped is restated about once a second — so a
    # peer who joins in the middle of this beat arrives to find the
    # others already on screen, without anyone having to waggle a mouse
    # to announce themselves.
    WHERE_Q = (880.0, 520.0)
    WHERE_X = (640.0, 610.0)
    ada.pointer_move(*WHERE_Q)
    ada.settle(0.6)
    # Moved, not filmed. Both halves of this are one fact seen from two
    # sides, and one side is enough to put on screen: ada's desktop
    # carrying bob's cursor, with nothing open on either machine, is
    # the picture. Cutting to bob to watch him move a mouse shows the
    # cause of something the audience has already been shown the effect
    # of.
    bob.pointer_move(*WHERE_X)
    bob.settle(0.6)

    def seen_by(watcher: Session, who: str):
        """Peer `who`'s pointer, as `watcher`'s session holds it.

        Both stores, because a person's pointer has lived in either. It
        was once filed into `pointers` *and* `remote_cursors`, which is
        what drew some peers twice; consolidating that left a reader of
        only one key looking at an empty list while the cursor was on
        screen, correctly drawn and correctly named. Reading both is the
        honest question — "does this session hold a pointer for that
        person" — rather than a question about where it is filed.

        A row is matched on either field, because the two carry
        different facts and only one of them is a person's name. The
        *identity* is `peer@<handle>`, derived from the handle the
        frames arrive under (`peer_presence.rs:420`); the *name* is what
        the room calls that identity, written into the room document
        under the inviter's authority. A peer that could announce its own
        name could announce anybody's, so identity stopped being the
        sending session's word for who is at it — and a predicate
        written against `user == "ada"` stopped matching anything at
        exactly that moment, which read here as presence failing while
        it was working.
        """
        peers = watcher.peers() or {}
        rows = []
        for key in ("pointers", "remote_cursors"):
            rows.extend(peers.get(key) or [])
        return [r for r in rows
                if who in (r.get("name"), r.get("user"))] or None

    def at_the_origin(row: dict) -> bool:
        """A pointer painted where nobody is.

        A roster entry that conjures a cursor at (0, 0) has been fixed
        once already. It is worth failing on rather than shooting around,
        because on screen it is indistinguishable from a peer who happens
        to have parked in the corner — and it is only wrong when the peer
        is demonstrably somewhere else, which is the one thing a harness
        can check and an eye cannot.
        """
        return abs(float(row.get("x", 0.0))) < 1.0 and \
            abs(float(row.get("y", 0.0))) < 1.0

    on_bob = wait_for(lambda: seen_by(bob, ada.user), timeout=SYNC_TIMEOUT_S)
    claim("bob's session paints ada's cursor on an empty desktop",
          bool(on_bob), True)
    if on_bob:
        row = on_bob[0]
        note(f"bob's screen holds {json.dumps(row)}")
        # The name is the other half of the beat, and it is a different
        # fact from the id: the id is the identity and is never blank,
        # the name is what the *room* calls that identity, and the pill
        # over the cursor draws the name. A cursor reading `peer-1` is a
        # cursor nobody can attribute to a person.
        #
        # Asserted on the *name*, and deliberately not on the id. The id
        # is `peer@<handle>` and is supposed to be: the sending session
        # does not get to say who is at it, or a peer could sit in a room
        # under somebody else's name. So "the label is trustworthy" is a
        # claim about where the name came from — the room document, which
        # only the room's authority can write — and this checks that the
        # id is a handle rather than a person's word for themselves.
        claim("the identity is the session's handle, not a name it chose "
              "for itself",
              str(row.get("user", "")).startswith("peer@"), True)
        claim("and the pill over it reads his name, resolved from the room",
              row.get("name"), ada.user)
        claim(f"and puts it where he actually is, not at the origin",
              at_the_origin(row), False)
        if at_the_origin(row):
            note(f"ada's pointer is at {WHERE_Q} and his peer row on "
                 f"bob's screen is at the origin — a cursor is being "
                 f"painted where he is not")
    bob.snapshot(os.path.join(shots, "03-bob-joined.png"))

    on_ada = wait_for(lambda: seen_by(ada, bob.user), timeout=SYNC_TIMEOUT_S)
    claim("and ada's paints bob's, the other way", bool(on_ada), True)
    if on_ada:
        claim("also at a real position", at_the_origin(on_ada[0]), False)
        note(f"ada's screen holds {json.dumps(on_ada[0])}")
    # Whether the agent publishes a pointer of its own is a fact about the
    # product, and it is read rather than assumed in either direction: an
    # earlier draft of this beat stated in a comment that it does not, and
    # a shot from that same run had a pill reading `Claude` in it.
    agent_seen = seen_by(ada, agent.user)
    if agent_seen:
        # Reported, not shot around, and not fixed here: the compositor
        # restates every session's pointer from its own frame loop, so a
        # session that has never had a pointer moved through it still
        # publishes one — and the agent is a session. Nothing in this
        # script moves it (there is no `agent.pointer_move` anywhere),
        # which is why this belongs to `shell` rather than to the film.
        gap("the agent publishes a desktop cursor: a pill reading "
            f"{agent.user!r} is painted on ada's screen at "
            f"{json.dumps(agent_seen[0])}. A pointer is for pointing at "
            f"something you are looking at, and this participant never "
            f"looks at a screen — its presence should be a caret in the "
            f"document at the moment it acts, and nothing else. The "
            f"pointer is level-triggered from the compositor's frame "
            f"loop, so this is a shell fix, not a demo one")
    else:
        note(f"{agent.user} publishes no desktop cursor, which is right: "
             f"what makes it a member of this room is the capability "
             f"above, and where it appears is in the document")
    read_hold()

    step("and they stay there without anyone moving")
    # The property that makes this beat filmable, asserted rather than
    # assumed. An edge-triggered cursor is published when it moves and
    # then forgotten, which looks identical to this one for a second and
    # then leaves an empty desktop behind — and means a peer arriving
    # later sees nobody until somebody jiggles a mouse. Nothing is moved
    # here on purpose: both pointers have been still since above.
    ada.settle(3.0)
    still_there = seen_by(ada, bob.user)
    claim("bob's cursor is still on ada's screen 3s after it stopped",
          bool(still_there), True)
    if not still_there:
        note("a stopped pointer is no longer being restated, so a peer who "
             "joins after the last mouse movement arrives to an empty room")


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
            "build it with `cargo build -p app-cli --bins --examples` "
            "and `cargo build -p tilo-install --bin tilo-install`")
        return None
    note(f"a registry at {registry.base_url} offers it")

    step("he searches for it, and the row is offered like any other")
    ada.modifiers(logo=True)
    ada.key("Space")
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
    # The one number in this film a viewer might time with a stopwatch,
    # so the camera gets out of the way while it is being taken. The
    # poll below queues behind the camera too, which is why it loosens
    # as well.
    with easing(4.0):
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
            time.sleep(0.25)
        took = time.time() - began
    claim("the word processor was fetched and opened", painted is not None, True)
    if painted is None:
        note(f"see {ada.log_path}")
        return None
    note(f"{took:.1f}s from Enter to first frame — one machine, one LAN hop, "
         f"an unoptimised payload; not a number to quote")
    claim("and it is installed now, on the machine that lacked it",
          WORD_PROCESSOR in set(installed_ids(ada.apps_dir)), True)
    # A group takes about three quarters of the strip by default, so the
    # second window opens mostly past the right edge — the strip is
    # working exactly as designed, and the app the beat is about is half
    # off screen while it does. Give every group an even share, so the
    # window this beat just opened is actually in the frame that shows it.
    frame_both_windows(ada, VIEWPORT)
    # A beat later than the first frame. `painted` means the app has
    # presented *something*, and what a `ui` app presents first is its
    # splash — so the shot that is supposed to show a word processor
    # arriving on a machine that lacked it was a title and a spinner.
    #
    # 2.5s was not enough of a beat: it produced exactly that shot, a
    # centred "Word Processor" over a dot spinner, in the frame the
    # letter's "an app appears with no install step" rests on. Held long
    # enough for the app to finish starting, and held *on camera* rather
    # than settled quietly, because an app coming up is worth watching
    # and this is the moment the beat is for.
    ada.settle(2.0)
    if RECORDER:
        RECORDER.hold(5.0)
    else:
        ada.settle(5.0)
    ada.snapshot(os.path.join(shots, "05-word-processor.png"))
    return painted


def beat_six(ada: Session, bob: Session, baseline: dict,
             opened: list[str], shots: str) -> str | None:
    banner("6.  bob's screen — the window, and what ada wrote in it")

    # Both halves of the rule get asserted, because either alone is easy
    # to satisfy wrongly: the window must arrive, *and* he must not be
    # moved to it.
    #
    # The baseline is taken before beat 5 opens anything, not here.
    # Measuring "a tab ada has that bob lacks" at this point asks
    # the question after the answer: replication is quick, and a run
    # where the window had already crossed reported that there was
    # nothing to watch — the success and the failure both came out as
    # silence. What is watched instead is the tab that did not exist
    # when the baseline was taken, which is the window beat 5 opened.
    #
    # No cut, and asked *before* beat 7 — because beat 7 has bob open a
    # word processor on his own machine, and opening an app moves your
    # focus to it. Asked afterwards, "his view did not move" is measured
    # across a window he opened himself: it failed exactly that way once,
    # reporting a replication bug that was a person clicking on their own
    # computer. The claim is that *somebody else's* window arriving does
    # not move you, so nothing of his own may happen in between.
    #
    # What the film shows is separate and later — see `show_bobs_screen`,
    # which frames the same screen once it is also carrying the sentence
    # ada typed.
    before_view = baseline["view"]
    before_groups = baseline["groups"]
    # Handed in, not diffed here. Since this beat moved after beat 7 there
    # is a second word processor in the room — the one bob opened on his
    # own machine — and a tab that is new since the baseline could be
    # either. Watching *his own* tab arrive in his own tree is a claim
    # that cannot fail, printed under a heading about replication. What is
    # watched is the tab ada's machine opened, identified before
    # bob opened anything.
    fresh = list(opened)
    note(f"ada holds {len(ada.tabs_here())} tab(s); bob holds "
         f"{len(bob.tabs_here())}; the tab under test is the one his "
         f"machine opened in beat 5, taken before bob opened his own")

    # Keep this beat on one of Tilo's own apps. Those replicate through
    # the room document and are drawn from it on the far side, which is
    # what makes a real window appear there. A foreign Wayland surface
    # has no such description to replicate and degrades to a card
    # reading "coming soon" — true to what the system can do today, and
    # the wrong thing to put in a shot whose whole claim is that the
    # window opened over there.
    step("the tab ada's word processor opened, read out of bob's tree")
    if not fresh:
        gap("beat 5 opened no window on ada's session, so there was "
            "nothing for this beat to watch cross")
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


def show_bobs_screen(bob: Session, shots: str) -> None:
    """The shot the film's one excursion was made for.

    Everything in it has already been proven — the window arrived in beat
    6, the sentence in beat 7 — and neither proof is a picture: both are
    answers to questions put over a socket. This is where a viewer gets
    to see the two of them on one screen at the same time, which is the
    only thing a second machine is worth crossing to.

    Framed rather than merely visited: an even share of the strip, every
    window whole, and then the document brought up in front. A previous
    cut ended its excursion on this session's file manager showing an
    empty folder, under a caption about a document two other participants
    had just written into.
    """
    frame_one_window(bob, WORD_PROCESSOR)
    read_hold(READ_HOLD_S * 2)
    bob.snapshot(os.path.join(shots, "06b-bob-window-and-text.png"))


def beat_seven(ada: Session, bob: Session, host_ws: dict,
               ada_wp: dict | None, shots: str) -> None:
    banner("7.  ada writes, and bob's screen is asked what it holds")
    room = host_ws["room"]

    # If beat 5 never got a word processor onto ada's machine there is
    # nothing here to type into, and typing anyway would send the text to
    # whatever happened to hold focus — which reads afterwards exactly
    # like a document that failed to cross. Say which of the two it was.
    if ada_wp is None:
        gap("ada has no word processor open (beat 5 did not put one "
            "there), so there was nothing for him to write in and nothing "
            "for this beat to watch cross")
        return

    step("bob already has the window — he opens nothing")
    # He does not launch anything here, and that is the point rather than
    # a shortcut. The window arrived on his machine in beat 6 because it
    # belongs to a room he is in; asking him to open a second copy of an
    # app he can already see would be staging work the product exists to
    # remove.
    #
    # It also used to be wrong in a way that hid itself. Opening the app
    # here started a *second* window on every machine in the room — one
    # locally-owned tab projects back into the room and every member
    # materialises it — so this beat quietly produced the duplicate
    # word processors that turned up in the frames. Since that was fixed,
    # a launch on his side focuses the window he has instead of making
    # one, so waiting for a new surface here waits for something that by
    # design never arrives.
    # Identified the same way the beat-6 assertion identifies it: a tab
    # on his machine whose title names the app. Deliberately not "any new
    # tab" — see beat 6 on why a tab he opened himself is a claim that
    # cannot fail.
    his = next(
        (t for t in bob.tabs_here().values()
         if (t.get("title") or "").lower().startswith("word processor")),
        None,
    )
    claim("the window that crossed in beat 6 is his to type in",
          his is not None, True)
    if his is None:
        gap("bob has no word processor on screen, so there is no far "
            "screen to read a document back off")
        return
    # With the viewport, so the framing is measured rather than performed
    # and assumed: this beat's shot is the far screen, and a far screen
    # that is half off the frame shows nothing about what it holds.
    frame_both_windows(bob, VIEWPORT)
    # Both apps have to have attached to their broker and announced their
    # address before either types, or the first keystrokes go out to
    # nobody and it reads as a transport failure.
    bob.settle(6.0)

    # What bob's screen looks like before a word of it exists. The
    # claim below is answered over a socket, and a socket cannot tell you
    # whether the app *drew* anything: a run once passed "bob's own
    # word processor holds what ada typed" with both pages on his
    # screen reading "Start writing…" for the whole beat. Assertion
    # green, screen blank — which is the failure this whole file is
    # written against, so it is checked rather than hoped for.
    before_screen = screen_shot(
        bob, os.path.join(shots, "07a-bob-before-the-text.png"))

    step(f"ada types {TYPED_TEXT!r}")
    cut("ada")
    # No click first, and that is the point rather than an omission.
    #
    # The word processor opens with its caret already in the body, so
    # there is nothing to put right — and clicking anyway can move focus
    # *off* the page onto the style panel beside it, which is a click
    # that lands squarely inside the window and still sends every
    # keystroke after it somewhere that is not the document. A beat that
    # clicked first read downstream as a document which failed to cross:
    # the wrong layer, and a much more alarming bug than the real one.
    #
    # If it turns out the caret was not there, the fallback below aims
    # one into the page and tries again — and the transcript says which
    # of the two actually wrote the text, because they are different
    # claims about how usable the app is.
    ada.type_text(TYPED_TEXT)
    ada.settle(1.5)

    def ada_holds() -> bool:
        """Ask his own app whether the text is in its document."""
        reader = None
        try:
            reader = DocumentReader(ada.collab_json_sock, room,
                                    record=None, handle="control")
            _doc, found = reader.await_document(TYPED_TEXT, 20.0)
            return bool(found)
        except OSError as e:
            note(f"ada's own session could not be asked ({e})")
            return False
        finally:
            if reader is not None:
                reader.close()

    typed_here = ada_holds()
    if typed_here:
        note("typed straight into the open document — the app had the "
             "caret in the page already, and nothing had to be aimed")
    else:
        note("nothing reached his document with no click, so the caret was "
             "not in the page; aiming one into it and typing again")
        # Same tile lookup as before. A client tile is registered as
        # `foreign-surface-<id>`, and when the compositor knows which tab
        # it belongs to it appends that in brackets — both forms occur in
        # one run, so the id is a prefix to match rather than a whole
        # name to look up. An exact-name lookup silently found nothing
        # while the window was plainly on screen.
        surface = (ada_wp or {}).get("surface_id")
        stem = f"foreign-surface-{surface}"
        named = next((n for n in ada.target_names()
                      if n == stem or n.startswith(stem + "[")), None)
        rect = ada.named_rect(named) if named else None
        if not rect:
            others = [n for n in ada.target_names()
                      if n.startswith("foreign-surface-")]
            gap(f"ada's word processor (surface {surface}) has no "
                f"laid-out tile either, so there is nowhere to aim a caret. "
                f"Laid out right now: {others or 'no client tiles at all'}")
        else:
            bx, by, bw, bh = rect
            at = (bx + bw * 0.4, by + bh * 0.55)
            note(f"his document is {named} at {[round(v) for v in rect]}; "
                 f"clicking {[round(v) for v in at]}")
            ada.click_at(*at)
            ada.settle(1.2)
            ada.type_text(TYPED_TEXT)
            ada.settle(1.0)
            typed_here = ada_holds()
    ada.snapshot(os.path.join(shots, "07-ada-typed.png"))

    step("first, that he typed into the document and not at the chrome")
    # The control, and the reason this beat can tell its two failures
    # apart. If the text never reaches bob, that is either a document
    # which did not cross or a document which never had the text in it,
    # and both end as an empty page on the far screen.
    claim("ada's own document holds what he typed", typed_here, True)
    if not typed_here:
        note("the keystrokes did not reach his document at all, so nothing "
             "downstream of this is about the network")

    step("and bob's session is asked what it now holds")
    # The one excursion in the film, and it stays open across beat 6:
    # this is the first moment a second machine has anything on it worth
    # crossing for. Everything before this — his redeeming the invite,
    # his opening his own copy of the app — happened while the camera
    # stayed with the person the story is about.
    cut("bob")
    # What is mechanically readable from here is presence: the caret and
    # pointer of the peer who is typing, attributed to his user id, off
    # the same context the overlay paints from.
    def ada_present():
        # Either field: the identity is `peer@<handle>` and the name is
        # what the room calls it. See `seen_by` in beat 4.
        rows = (bob.peers() or {}).get("pointers") or []
        return [r for r in rows
                if ada.user in (r.get("name"), r.get("user"))] or None

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
        # Filmed for as long as it usually takes, and no longer. When it
        # works the sentence turns up on his page in a few seconds and
        # that is the shot; when it does not, a minute of an unchanged
        # page under a caption about a document arriving is the worst
        # thing this film could contain.
        with patience(10.0):
            doc, carried = reader.await_document(TYPED_TEXT,
                                                 DOC_SYNC_TIMEOUT_S)
    except OSError as e:
        gap(f"bob's session has no JSON broker to ask ({e}) — "
            f"looked at {bob.collab_json_sock}")
    finally:
        if reader is not None:
            reader.close()

    # Which of the three outcomes this was decides how it is recorded,
    # and the distinction is not cosmetic. A document that came back
    # without the text is a question that was *asked and answered*: the
    # claim is false, it counts as a failure, and it gates the exit code
    # like every other claim here. Silence is a different thing — nothing
    # was established either way — and that is what a GAP is for.
    if doc is not None:
        claim("bob's own word processor holds what ada typed",
              carried, True)
        if carried:
            note(f"answered with a {len(doc)} byte document carrying "
                 f"{TYPED_TEXT!r} — produced by his app out of the "
                 f"document it holds, not read back off the wire")
        else:
            note(f"his app answered with {len(doc)} bytes and none of it "
                 f"is {TYPED_TEXT!r}: the two apps are attached and "
                 f"answer each other's handshake, and their documents "
                 f"are still two documents. Presence crosses, the "
                 f"document does not, and nothing on his screen shows "
                 f"what ada wrote")
    else:
        gap(f"no app in bob's session answered the late-join handshake "
            f"within {DOC_SYNC_TIMEOUT_S:.0f}s, so his document was never "
            f"read back — this run establishes nothing about beat 7 in "
            f"either direction. See {bob.log_path}")
    bob.settle(2.0)
    after_screen = screen_shot(
        bob, os.path.join(shots, "08-bob-received.png"))

    # The cross-check. Only meaningful when the socket said yes: a
    # document that never arrived is allowed to leave the screen alone.
    if carried:
        moved = pixels_moved(before_screen, after_screen)
        if moved is None:
            gap("bob's session answered with the text, and his screen "
                "could not be compared, so whether his app drew any of "
                "it is not established by this run")
        elif moved < SCREEN_MOVED_PX:
            gap(f"bob's session answered with a document carrying "
                f"{TYPED_TEXT!r}, and {moved} pixels of his screen moved "
                f"while it arrived — a blinking caret, and no text. His "
                f"pages read 'Start writing…' either side of it. So the "
                f"app that answered is not the app that is drawn: the "
                f"broker computes an addressed channel key and fans out "
                f"room-wide anyway (grep `room_wide=true` in "
                f"{bob.log_path}), so a handshake addressed to the "
                f"word processor is answered by whatever else is in the "
                f"room, and this claim passes without his word processor "
                f"holding anything. It is the same defect that leaves "
                f"the agent waiting in beat 9, and this is the shot the "
                f"film crosses to a second machine to get")
        else:
            note(f"and {moved} pixels of his screen moved while it "
                 f"arrived, so the app that answered is the app that is "
                 f"drawn")


#: Where the file view puts its rows, straight off the app's own
#: constants (`views/file_view.rs:63`): a 30px column header, then rows
#: of 34px. Used only to convert a press that landed on the *wrong* row
#: into an aimed one — never to guess where a row is in the first place.
LIST_ROW_H = 34.0
LIST_HEADER_H = 30.0


def expected_listing(home: str) -> list[str]:
    """The order Files draws this directory in: folders first, then
    files, each case-insensitively by name.

    Only used to work out how many rows separate the row a press landed
    on from the one that was wanted. Being wrong about it costs a
    correction, not the beat — every press is checked against what the
    app says it hit.
    """
    try:
        names = [n for n in os.listdir(home) if not n.startswith(".")]
    except OSError:
        return []
    dirs = sorted((n for n in names if os.path.isdir(os.path.join(home, n))),
                  key=str.lower)
    files = sorted((n for n in names if not os.path.isdir(os.path.join(home, n))),
                   key=str.lower)
    return dirs + files


def press_the_photo_row(ada: Session, log: LogTail, tile: tuple,
                        photo_name: str, listing: list[str]) -> tuple | None:
    """Press on the photograph's row and *keep the button down*.

    The whole difficulty of this beat is in this function. A file row
    belongs to a client, and a client's widgets are not in the
    compositor's registry, so there is no name to click and no rect to
    read — the press has to be aimed at a pixel. Aiming at a remembered
    pixel is what went wrong repeatedly, so nothing here is remembered:

    - the tile's rect comes from `foreign_frames`, which is the
      placement the compositor is painting right now;
    - every press is checked against the app's own account of what it
      hit (`drag armed path=… at=…`), so a miss is *known* to be a miss
      rather than assumed to be a hit;
    - a press that lands on the wrong row is corrected using the
      client-space y the app just reported, not using arithmetic about
      where the tile's chrome ends. One press measures the mapping
      between screen and client coordinates; the second uses it.

    Returns the (x, y) the button is held at, or None with the button
    up and nothing selected.
    """
    tx, ty, tw, th = tile
    # The name column: right of the sidebar, left of the size column.
    x = tx + tw * 0.45
    want = photo_name.lower()
    try:
        target_index = [n.lower() for n in listing].index(want)
    except ValueError:
        target_index = None

    def press(at_y: float) -> tuple[str, float] | None:
        """Press at `at_y`; report what the app says it grabbed."""
        ada.pointer_move(x, at_y)
        ada.settle(0.15)
        log.read()
        ada.pointer_button(True)
        # The press has to cross a process boundary, be dispatched by
        # the app's event loop and be written to a log this then reads.
        # 0.5s is the difference between "the app did not report it" and
        # "the app had not reported it yet".
        ada.settle(0.5)
        hit = log.armed(log.read())
        return (hit[0], hit[2]) if hit else None

    # Where to try first. Rows start below the app's toolbar and the
    # column header, and how much chrome sits above them depends on what
    # the app stacked there — so this sweeps a band rather than trusting
    # a single offset. Coarse, because it only has to land on *any* row;
    # the correction below does the aiming.
    for frac in (0.16, 0.22, 0.10, 0.28, 0.34):
        y = ty + th * frac
        hit = press(y)
        if hit is None:
            # Nothing under it — empty space below the last row, or the
            # toolbar. Let go before trying elsewhere, or the next press
            # arrives with a button already down.
            ada.pointer_button(False)
            ada.settle(0.2)
            continue
        path, client_y = hit
        if os.path.basename(path).lower() == want:
            note(f"pressed on {os.path.basename(path)} at y={y:.0f}")
            return (x, y)
        # A row, but not the one. The app reported the press's position
        # in its own coordinates, so the offset between the two spaces
        # is now measured rather than assumed, and one jump of N rows
        # lands on the wanted one.
        if target_index is None:
            ada.pointer_button(False)
            ada.settle(0.2)
            break
        try:
            hit_index = [n.lower() for n in listing].index(
                os.path.basename(path).lower())
        except ValueError:
            ada.pointer_button(False)
            ada.settle(0.2)
            continue
        offset = y - client_y          # screen y  =  client y + offset
        aimed = (client_y + (target_index - hit_index) * LIST_ROW_H + offset)
        note(f"the press landed on {os.path.basename(path)} "
             f"(row {hit_index}); {photo_name} is row {target_index}, "
             f"{abs(target_index - hit_index)} rows "
             f"{'down' if target_index > hit_index else 'up'} at "
             f"y={aimed:.0f}")
        ada.pointer_button(False)
        ada.settle(0.3)
        again = press(aimed)
        if again and os.path.basename(again[0]).lower() == want:
            note(f"pressed on {os.path.basename(again[0])} at y={aimed:.0f}")
            return (x, aimed)
        ada.pointer_button(False)
        ada.settle(0.2)
        break
    return None


def beat_eight(ada: Session, files: dict | None, wp: dict | None,
               shots: str) -> None:
    banner("8.  a photograph dragged out of Files and into the document")
    # Named while the camera is down, so the film comes back up on
    # ada rather than on the screen the excursion left behind.
    cut("ada")
    # Run, asserted, and left out of the cut — see `run`. It is the
    # longest beat here by a distance, most of that spent moving a
    # pointer slowly enough for two processes to hand a drag between
    # them, and none of the claims this film is cut to support is about
    # drag and drop.
    #
    # What makes this beat worth running is not that a picture appears in
    # a document. It is *how* it gets there: Files and the word processor
    # are two sandboxed clients with two different mount namespaces, so
    # the path Files is holding means nothing in the other app's world.
    # The drag carries the file's **bytes**, not its name — Files reads
    # its own file at the start of the gesture, because it has the
    # authority to read what it is showing, the compositor brokers a
    # pipe, and the receiving toolkit spools what comes down it into its
    # own directory. Neither app is ever given a path into the other's.
    note("two sandboxed clients, two mount namespaces: the drag carries "
         "the file's bytes, not a path either app could resolve")

    photo = os.path.join(ada.home, PHOTO_NAME)
    claim("there is a photograph in his home to drag",
          os.path.exists(photo), True)
    if not os.path.exists(photo):
        return
    payload = open(photo, "rb").read()

    # Both windows whole, before anything is aimed at either of them. A
    # drag that starts or ends off the edge of the screen is not a drag
    # anybody can see.
    unpainted = wait_until_painted(ada)
    if unpainted:
        gap(f"the shell is not painting {unpainted} yet, so this would be "
            f"filmed against an empty desktop")
    frame_both_windows(ada, VIEWPORT)
    ada.settle(0.5)
    tiles = painted_tiles(ada)
    if VIEWPORT:
        clipped = windows_off_screen(ada, VIEWPORT)
        claim("both windows are wholly on screen before the gesture",
              clipped, [])
        if clipped:
            gap("a window is cut off, so the gesture would cross an edge "
                "the camera cannot show — " + "; ".join(clipped))
            ada.snapshot(os.path.join(shots, "09-not-in-frame.png"))
            return

    def tile_for(app_id: str, painted: dict | None) -> tuple | None:
        sid = (painted or {}).get("surface_id")
        if sid is not None and int(sid) in tiles:
            return tiles[int(sid)]["rect"]
        # No surface id to hand (an earlier beat did not return one):
        # fall back to the app id, which is unambiguous while there is
        # one window per app on screen.
        for t in tiles.values():
            if t["app_id"] == app_id:
                return t["rect"]
        return None

    files_tile = tile_for(FILES_APP, files)
    wp_tile = tile_for(WORD_PROCESSOR, wp)
    # And that they are not the same window seen twice, nor two windows
    # stacked. `tile_for` falls back to matching on app id when the
    # surface an earlier beat opened is no longer laid out, which is how
    # this beat once aimed a drag between a full-screen file manager and
    # a word processor drawn underneath it, having just asserted that
    # both were wholly on screen.
    stacked = windows_overlapping(ada)
    claim("and neither window is drawn on top of the other", stacked, [])
    if stacked:
        gap("the two windows this gesture goes between are stacked, so a "
            "drag across them is not a gesture the camera can show — "
            + "; ".join(stacked))
        ada.snapshot(os.path.join(shots, "09-not-in-frame.png"))
        return
    claim("Files is on screen to drag out of", files_tile is not None, True)
    claim("the word processor is on screen to drop into",
          wp_tile is not None, True)
    if not files_tile or not wp_tile:
        gap("both windows have to be on screen at once for this gesture; "
            f"the session is painting {[t['app_id'] for t in tiles.values()]}")
        return
    note(f"Files at {[round(v) for v in files_tile]}, "
         f"the document at {[round(v) for v in wp_tile]}")
    ada.snapshot(os.path.join(shots, "09-before-drag.png"))

    log = LogTail(ada.log_path)
    log.catch_up()

    # A blank line at the end of the document for the photograph to land
    # on. Without one it goes in wherever along the existing text the
    # pointer happens to be over, which is the middle of a word about as
    # often as not.
    #
    # Aimed at 30% down, not 20%: the top band of the page is the header
    # zone, and a click there opens header editing — which paints a
    # labelled dashed box across the top of the document and stays on
    # screen for the rest of the beat.
    wx0, wy0, ww0, wh0 = wp_tile
    ada.click_at(wx0 + ww0 * 0.30, wy0 + wh0 * 0.30)
    ada.settle(0.8)
    ada.key_down("Control")
    ada.key("End")
    ada.key_up("Control")
    ada.key("Enter")
    ada.settle(0.6)

    step(f"he presses on {PHOTO_NAME}")
    # A click first, so the press that starts the drag is not the one
    # that focuses the window. The first click on an unfocused client is
    # spent on the focus change and never reaches the app's own widgets —
    # measured, and the cause of a drag that produced nothing at all.
    fx, fy, fw, fh = files_tile
    ada.click_at(fx + fw * 0.45, fy + fh * 0.16)
    ada.settle(0.8)

    listing = expected_listing(ada.home)
    note(f"his home lists as {listing}")
    held = press_the_photo_row(ada, log, files_tile, PHOTO_NAME, listing)
    claim(f"the press landed on {PHOTO_NAME}'s own row", held is not None, True)
    if held is None:
        gap(f"no press landed on {PHOTO_NAME}: the file rows belong to a "
            f"client and are not addressable by name from the compositor, "
            f"so this is aimed at pixels and the app did not report a row "
            f"under any of them. See {ada.log_path}")
        ada.snapshot(os.path.join(shots, "09-missed-the-row.png"))
        return
    hx, hy = held

    step("and drags it across the desktop")
    # Past the app's own drag threshold first, slowly, while the button
    # is still down. That is the moment Files opens a `wl_data_source`
    # and hands the gesture to the compositor; a drag that teleports to
    # its destination never gives either process a frame in which to do
    # it, and nothing is ever picked up.
    for step_px in (4, 10, 18, 30):
        ada.pointer_move(hx + step_px, hy - step_px * 0.4)
        ada.settle(0.12)
    promoted = log.await_line(LogTail.CARRIED, 3.0)
    claim("Files handed the gesture out as a client drag", promoted, True)
    if promoted:
        note("from here the picture on screen belongs to the compositor: "
             "the source window will not see another motion event, and "
             "will never see the release")
    took = log.await_line(LogTail.PROMOTED, 3.0)
    claim("and the compositor took it", took, True)

    # Across, in steps, with the chip drawn at the cursor the whole way.
    #
    # Where it is released matters more than it looks. The word
    # processor moves its caret to the drop point before inserting
    # (`widget.rs:5792`), clamping the pointer to the nearest line and
    # then picking a character position along it — so a drop over the
    # middle of the page lands the photograph in the middle of a *word*.
    # The first take of this beat cut "and" in half and left a
    # spell-check squiggle on screen.
    #
    # An empty last line was opened above, so releasing *below* all the
    # text clamps to that line, where there is no character to land in
    # the middle of. Left of centre keeps it off the styles panel, which
    # the drop handler ignores entirely.
    wx, wy, ww, wh = wp_tile
    drop = (wx + ww * 0.30, wy + wh * 0.55)
    x0, y0 = hx + 30, hy - 12
    frames = 26
    for i in range(1, frames + 1):
        t = i / frames
        ada.pointer_move(x0 + (drop[0] - x0) * t,
                             y0 + (drop[1] - y0) * t)
        ada.settle(0.05)
        if i == frames // 2:
            # Mid-flight, over open desktop between the two windows:
            # this is the frame that shows the drag is the OS's and not
            # either app's.
            ada.snapshot(os.path.join(shots, "10-carrying.png"))
    ada.settle(0.4)
    ada.snapshot(os.path.join(shots, "11-over-the-page.png"))

    step("and drops it on the page")
    log.read()
    ada.pointer_button(False)

    # The payoff, off the receiving app's own account of its own
    # document. An image is stored as its own bytes under a BLAKE3 hash,
    # so it moves two orders of magnitude more than a keystroke does —
    # there is nothing in between for this to confuse.
    want = max(4000, len(payload) // 2)
    biggest = 0
    deadline = time.time() + 25.0
    while time.time() < deadline:
        biggest = max(biggest, log.biggest_delta(log.read()))
        if biggest >= want:
            break
        time.sleep(0.3)
    landed = biggest >= want
    claim("the document changed by the size of a photograph, not a "
          "keystroke", landed, True)
    note(f"largest change the word processor broadcast after the drop: "
         f"{biggest} bytes (the file is {len(payload)}; a keystroke moves "
         f"about a hundred)")
    ada.settle(2.0)
    ada.snapshot(os.path.join(shots, "12-image-in-the-document.png"))

    if not landed:
        gap(f"nothing the size of the photograph reached the document "
            f"within 25s of the drop. See {ada.log_path}")
        return

    step("and it is in the document, not merely on the screen")
    # Asked of the app, after the camera has moved on: a late joiner on
    # this session's broker gets the whole document back, and the
    # photograph's own bytes are either in it or they are not.
    room = ada.active_workspace()["room"]
    carried, size_bytes = image_is_in_the_document(ada, room, payload)
    claim("the word processor's own document carries the photograph's "
          "bytes", carried, True)
    if carried:
        note(f"answered with a {size_bytes} byte document holding the "
             f"file's own header — so the image replicates the way the "
             f"text does, and a peer receives the picture rather than a "
             f"path to it")
    elif size_bytes:
        gap(f"the app answered with {size_bytes} bytes and the "
            f"photograph's are not among them")
    else:
        gap("the app never answered the late-join handshake, so the "
            "document could not be read back and the delta above is the "
            "only account of the image landing")


def beat_nine(ada: Session, bob: Session, agent: Session,
              host_ws: dict, ada_wp: dict | None, shots: str) -> None:
    banner("9.  the agent acts, and the room shows who did it")
    # The camera stays on the people. An agent is a participant in
    # somebody else's room, so what it does is shown the way any other
    # collaborator's actions are shown — arriving on their screens — and
    # never as a desktop of its own. Everything it is asked below is
    # asked over its control socket and printed to the transcript; none
    # of it is filmed, because there is nothing there to film.
    cut("ada")

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
    # Deliberately not filmed: this one opened a panel on the agent's own
    # session, and the agent's own session is the one screen this film
    # does not show.

    step("no synthetic input was used to do it")
    claim("no keystroke, click or modifier reached the agent since",
          agent.input_ops, baseline)
    note("the address (provider, id) is the entire input to the invocation")

    # The action above was a shell action, so it changed the shell: it
    # opened a panel on the agent's own session. The document is reached
    # the other way round, and the next step is that — not a lesser
    # substitute for it.
    #
    # `tilo-agent` joins the room's collaboration broker as an ordinary
    # CRDT peer and applies `WordProcessorAction` — the same variants the
    # word processor's own toolbar dispatches — under its own `ActorId`,
    # then broadcasts the result. So the exported action is the
    # *vocabulary* and the CRDT is the *transport*: what arrives on the
    # other two screens is the app's own operation, attributable to the
    # participant that made it. And it holds no screen while it does —
    # there is no winit, wgpu or vello anywhere in `apps/agent`.
    note("what it just invoked was a shell action, so it moved the shell. "
         "The document is reached as a participant: apps/agent applies "
         "the word processor's own actions under its own actor and "
         "broadcasts them, with no window of its own")

    # A terminal used to be opened here, because opening a window was
    # the only visible thing an agent could do by address. It ended the
    # film on a `bash` prompt in a fourth window — a developer machine,
    # in the last shot a viewer keeps, arguing against the strip the
    # letter points at. What follows is the beat that was wanted
    # instead: the agent changes the document the other two are looking
    # at, and it is read back off somebody else's screen.
    step("ada writes a list, and asks Claude for a table")
    cut("ada")
    if not type_into_the_document(ada, ada_wp, AGENT_BULLETS):
        gap("nothing could be typed into ada's document, so there is "
            "no list for the agent to act on")
        return
    ada.settle(1.0)
    read_hold()
    ada.type_prose(AGENT_INSTRUCTION)
    ada.settle(1.5)
    # What he asked for, in his own document, addressed to a participant
    # by name. Nothing else in the film explains what is about to happen,
    # so this frame has to be readable.
    read_hold(READ_HOLD_S * 2)
    ada.snapshot(os.path.join(shots, "11-instruction.png"))
    note(f"the instruction is a line in the document: {AGENT_INSTRUCTION!r}")

    step("the agent reads it and carries it out — with no window at all")
    # The best fifteen seconds in the film when it works — a table
    # assembling itself in a document nobody is touching — and the worst
    # two minutes in it when it does not. Bounded accordingly.
    with patience(15.0):
        ran = run_the_agent(agent, host_ws["room"], shots)
    if ran is None:
        gap("tilo-agent is not built, so the agent could not act on the "
            "document (cargo build -p agent --bin tilo-agent)")
        return
    claim("the agent carried out the instruction", ran, True)
    claim("and no synthetic input reached its session to do it",
          agent.input_ops, baseline)

    step("and bob's own word processor is asked what it holds now")
    # The payoff, read off the screen of somebody who did not make the
    # change. The assertion is that the *content* survived into the
    # table: a table with the rows empty is what this beat produced
    # before, and it is indistinguishable from success in a transcript
    # that only asks whether a table exists.
    #
    # Asked of bob's session and *filmed on ada's*. That a third
    # participant's edit reached a fourth machine is the strongest form
    # of the claim and is worth asserting; it is not worth a second
    # excursion, because the same edit is arriving on the screen already
    # on camera, in the document the audience has watched being written.
    # An agent's work showing up in your document the way a colleague's
    # does is the whole point, and it is a point about *your* screen.
    reader = None
    try:
        reader = DocumentReader(bob.collab_json_sock, host_ws["room"],
                                record=None)
        # A question over a socket, with the camera on a screen it is not
        # about. Nothing here is worth a minute of film in either
        # direction.
        with patience(3.0):
            doc, carried = reader.await_document(AGENT_BULLET_PROOF,
                                                 DOC_SYNC_TIMEOUT_S)
    except OSError as e:
        gap(f"bob's session has no JSON broker to ask ({e})")
        doc, carried = None, False
    finally:
        if reader is not None:
            reader.close()
    if doc is not None:
        claim("bob's document carries the rows the agent made, with the "
              "text still in them", carried, True)
    else:
        gap("bob's app never answered, so what his screen holds was not "
            "established")
    # The last shot of the film, so it is framed like one: even shares,
    # the strip scrolled back, and every window whole. An accumulation
    # of everything the run happened to open is the picture that argues
    # against the layout this is meant to show off.
    #
    # …and then pointed at the subject. Framing the strip is not the same
    # as choosing what the shot is of: an earlier cut ended on a file
    # manager showing an empty folder, under a caption about a document
    # two other participants had just written into.
    frame_one_window(ada, WORD_PROCESSOR, prefer=ada_wp)
    read_hold(READ_HOLD_S * 2)
    ada.snapshot(os.path.join(shots, "12-agent-table.png"))
    bob.snapshot(os.path.join(shots, "12b-bob-agent-table.png"))


def run(ada: Session, bob: Session, agent: Session, shots: str,
        registry) -> None:
    """Every beat, in order — and which of them the camera is up for.

    All nine run and all nine are asserted; the transcript is the same
    either way. What differs is the cut, and the cut is decided here
    rather than inside the beats, so the shape of the film is one
    readable page instead of nineteen scattered decisions.

    The shape is one excursion: ada's screen for the length of the
    story, out to bob once at the moment his machine is holding
    something worth crossing for — a window he never opened, carrying a
    sentence he never typed — and back. Two cuts. The previous version
    had eleven, and a stranger watching it spent most of them working out
    whose machine they were on instead of watching what happened on it.

    The other two participants are visible *through* ada's screen
    rather than by going to theirs: bob arrives as a row in a roster
    and a cursor on the backdrop, and the agent as a chip in that same
    roster and then an edit landing in the document on camera. Which is
    also what collaboration software looks like when it works — you stay
    on your own screen.

    The beats that are run but not filmed are not filler — they are the
    proof for claims the film makes elsewhere, or beats good enough to
    deserve their own minute rather than fifteen seconds of this one.
    """
    host_ws, files = beat_one(ada, shots)
    # ada raises the invitation on camera, at length, because it is
    # the gesture the whole collaboration story hangs off. bob redeeming
    # it is a person pasting a string into a dialog, and happens off
    # camera inside the beat.
    beat_two(ada, bob, host_ws, shots)
    with off_camera("the agent's invitation — the same card, a different "
                    "name on it — and the permission check that follows, "
                    "which is a question to the authority gate and has no "
                    "picture"):
        beat_three(ada, agent, host_ws, shots)
    # Everything bob holds and is looking at *before* the word
    # processor exists anywhere. Beat 6 measures against this rather than
    # against the state it finds when it starts, so "the window crossed
    # before we looked" reads as the success it is.
    baseline = {
        "tabs": set(bob.tabs_here()),
        "view": bob.view_state(),
        "groups": bob.groups_here(),
    }
    beat_four(ada, bob, agent, host_ws, shots)
    wp = beat_five(ada, registry, shots)
    # Which tab beat 5 opened, read now — before beat 7 puts a second
    # word processor in the room and makes "new since the baseline"
    # ambiguous. Beat 6 is asked about this one specifically.
    opened = [t for t in ada.tabs_here() if t not in baseline["tabs"]]
    note(f"beat 5 opened {len(opened)} tab(s) on ada's machine: "
         f"{opened or 'none'}")
    # Proven here, seen later. Beat 6 has to run before beat 7 — beat 7
    # has bob open an app on his own machine, and that moves his own
    # focus, which is the thing beat 6 asserts nothing did. Both its
    # claims are answers over a socket rather than pictures, so running
    # it off camera costs the film nothing.
    with off_camera("bob's tree, read over a socket: the window is there "
                    "and his view did not move. His screen is seen holding "
                    "it, and the sentence, in the excursion below"):
        beat_six(ada, bob, baseline, opened, shots)
    # The excursion: beat 7 opens it at its own last step, once his screen
    # has the sentence on it too, and the shot closes it. Beat 9 cuts back.
    beat_seven(ada, bob, host_ws, wp, shots)
    show_bobs_screen(bob, shots)
    with off_camera("the photograph dragged between two sandboxes: the "
                    "longest beat here, and not one of the things this "
                    "cut is arguing"):
        beat_eight(ada, files, wp, shots)
    beat_nine(ada, bob, agent, host_ws, wp, shots)




# --------------------------------------------------------------------


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", default=os.path.join(REPO, "target", "yc-demo"),
                    help="run directory for state, logs and screenshots")
    ap.add_argument("--shell",
                    default=os.path.join(REPO, "target", "demo", "shell"),
                    help="path to the compositor binary; must be a build that "
                         "still honours TILO_TEST_CONTROL (any profile with "
                         "debug-assertions on — `demo` or `dev`, never "
                         "`--release`)")
    ap.add_argument("--allow-stale-shell", action="store_true",
                    help="record even if the compositor binary is older than "
                         "the sources under test (default: refuse, because a "
                         "frame cannot tell you which build it came from)")
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
    ap.add_argument("--record-dwell", type=float, default=0.3, metavar="S",
                    help="seconds to hold on each beat and step while "
                         "recording. Real elapsed time, really sampled — "
                         "never a held frame standing in for one. Small, "
                         "because it is paid on every step in the run and "
                         "the frames that actually have to be *read* ask "
                         "for their time separately (`read_hold`)")
    ap.add_argument("--record-cut-dwell", type=float, default=1.2, metavar="S",
                    help="seconds to hold after cutting to a session, so the "
                         "screen you have just been moved to registers "
                         "before the demo carries on. There are three cuts "
                         "in the film, so this is cheap and worth having")
    ap.add_argument("--record-max-hold", type=float, default=0.6, metavar="S",
                    help="longest one *accidentally* unchanged frame is "
                         "shown for. A session waiting on a sandbox spawn "
                         "or a network hop renders the same screen for as "
                         "long as it waits; this trims that dead air. "
                         "Nothing is duplicated or interpolated — a frame "
                         "is only ever shown for less time than it was "
                         "live. Frames the demo deliberately stopped on "
                         "(`read_hold`) are exempt, so this can be tight "
                         "without shortening the card somebody has to "
                         "read. 0 keeps the video on wall-clock time")
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
              f"cargo build --profile demo -p shell -p tilo-control --bin shell --bin tilo-control",
              file=sys.stderr)
        return 2

    # A recording wants the size that tiles into the video 1:1; a plain
    # run wants a desktop-shaped viewport. An explicit --size beats both.
    size = args.size or (args.record_size if args.record is not None
                         else "1280x800")

    # Absolute, always. A relative --out reaches the compositor as a
    # relative XDG_CONFIG_HOME, which `dirs::config_dir()` ignores, and
    # the three sessions then persist into the real ~/.config/tilo rather
    # than into this run.
    args.out = os.path.abspath(args.out)

    # Always from a clean slate. Sessions persist their workspaces, so a
    # re-run over a previous run's state starts with rooms already joined
    # and a catalogue that no longer offers `peer.connect` — the demo
    # then fails on a stale precondition rather than on anything real.
    # `--keep` is about teardown, never about reusing state.
    shutil.rmtree(args.out, ignore_errors=True)
    # The homes are throwaway too, and they live outside `--out` (see
    # HOMES_ROOT), so they need clearing on their own. A home left over
    # from a previous run carries that run's edits into this one's first
    # shot.
    shutil.rmtree(HOMES_ROOT, ignore_errors=True)
    shots = os.path.join(args.out, "shots")
    os.makedirs(shots, exist_ok=True)

    shadowed = _shadowed_definitions()
    if shadowed:
        print("\nrefusing to record: this file defines a name twice, so a "
              "call site is reaching a function it does not mean")
        for d in shadowed:
            print(f"    {d}")
        return 2

    # Refuse to film a binary older than the code it is meant to show.
    #
    # This rig has silently recorded a stale compositor more than once:
    # the run is green, the frames are of last night's build, and the
    # fix everyone is waiting to see is simply not in the picture. A
    # screenshot cannot tell you which build it came from, so the check
    # has to happen here, before anything is recorded.
    #
    # Sources are compared against the binary's own mtime. Newer source
    # is refused rather than warned about: a warning in a 300-line
    # transcript is a warning nobody reads.
    if not args.allow_stale_shell:
        newer = []
        try:
            built = os.path.getmtime(args.shell)
            for root in (os.path.join(REPO, "shell", "src"),
                         os.path.join(REPO, "ui", "src")):
                for dirpath, _dirs, files in os.walk(root):
                    for f in files:
                        if not f.endswith(".rs"):
                            continue
                        fp = os.path.join(dirpath, f)
                        try:
                            if os.path.getmtime(fp) > built:
                                newer.append(os.path.relpath(fp, REPO))
                        except OSError:
                            pass
        except OSError as e:
            print(f"cannot stat {args.shell}: {e}")
            return 2
        if newer:
            newer.sort()
            print(f"\nrefusing to record: {os.path.relpath(args.shell, REPO)} "
                  f"is older than {len(newer)} source file(s) under test")
            for f in newer[:8]:
                print(f"    {f}")
            if len(newer) > 8:
                print(f"    ... and {len(newer) - 8} more")
            print("\n  cargo build --profile demo -p shell -p tilo-control --bin shell "
                  "--bin tilo-control")
            print("  (or --allow-stale-shell to film it anyway)")
            return 2

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
        ("TILO_APP_BIN", "app"),
        ("TILO_INSTALL_BIN", "tilo-install"),
        ("TILO_LAUNCHER_BIN", "tilo-launcher"),
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
    for short, name in zip("qxa", ("ada", "bob", "Claude")):
        run_dir = os.path.join(args.out, name)
        apps_dir = os.path.join(run_dir, "tilo-apps")
        # ada is the machine that does not have it yet. Everyone
        # else does, so bob can open his own copy in beat 7.
        compose_registry(apps_dir, entries,
                         drop=(WORD_PROCESSOR,) if name == "ada" else ())
        sessions.append(Session(
            name, name, run_dir,
            os.path.join(runtime, f"tilo-yc-{short}-{pid}.sock"),
            args.shell, size,
            # A private runtime dir per session, standing in for the
            # per-user one logind would give each real login.
            os.path.join(runtime, f"qyc-{short}{pid}"),
            stage_home(os.path.join(HOMES_ROOT, name)),
            apps_dir, tools))
    ada, bob, agent = sessions
    global SESSIONS, VIEWPORT
    SESSIONS = sessions
    VIEWPORT = tuple(int(v) for v in size.lower().split("x"))

    print(f"compositor:    {args.shell}")
    print(f"run directory: {args.out}")
    print(f"apps:          {len(entries)} installed for bob and the agent, "
          f"{len(entries) - (1 if WORD_PROCESSOR in entries else 0)} for ada")
    for var, path in sorted(tools.items()):
        print(f"  {var:20} {path}")
    for var in ("TILO_APP_BIN", "TILO_INSTALL_BIN", "TILO_LAUNCHER_BIN"):
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
                print(f"  {s.name:8} TILO_TEST_CONTROL={s.sock}  "
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
        try:
            RECORDER.beat_report(BEAT_MARKS, args.record_fps)
        except Exception as exc:  # noqa: BLE001 — never lose the video for it
            print(f"  (could not attribute video time to beats: {exc})")
        # The claim the whole film rests on, checked against the frames
        # rather than trusted to the beats. `cut` refuses the name, but
        # `cut` is not the only way `_live` could come to point at that
        # session, and this is the check that still holds when somebody
        # adds another way. A statistic reading "Claude 0 frames" is
        # something a person has to notice; this fails the run.
        strays = RECORDER.frames_from(NEVER_ON_CAMERA)
        if strays:
            FAILURES.append(
                f"{strays} frame(s) in the recording were captured from "
                f"{NEVER_ON_CAMERA}'s session — an agent's desktop is the "
                f"one thing this film must not contain")
        else:
            print(f"  no frame came from {NEVER_ON_CAMERA}'s session: what "
                  f"it did is in the film only where the people it did it "
                  f"with can see it")
        # Composed after teardown: encoding three tracks is CPU-hungry,
        # and the sessions have nothing left to say by now.
        try:
            out_video = os.path.join(record_dir, "tilo-demo.mp4")
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
