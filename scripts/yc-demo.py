#!/usr/bin/env python3
"""Three qOS sessions in one room, and an agent that drives one of them.

Starts three real compositors — separate state directories, separate
libp2p keypairs, separate user identities, separate control sockets:

  quinten  hosts the room and owns it
  xaver    joins it by invite
  Claude   is admitted to it too, and from then on is driven *only* by
           the action API: semantic invocation by `(provider, id)`,
           never by a click, a keystroke, or a pixel

The storyboard, in order, about a minute:

  1. an empty desktop, and Files opened on it
  2. quinten invites xaver
  3. quinten invites the agent — a capability principal, not a peer
  4. both have joined, and each session can see the others
  5. "word processor" — a machine that does not have it, fetching it
  6. cut to xaver: the window quinten opened is open for him too
  7. quinten writes, and xaver's screen is asked what it holds
  8. a photograph dragged out of Files and into the document
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

import ast
import argparse
import contextlib
import hashlib
import base64
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
from reaper import die_with_parent, track   # noqa: E402

# Set from `--keep`. Off means the kernel reaps a compositor with this
# script, however this script dies.
KEEP_SESSIONS = False

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


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
        path = os.path.join(REPO, "target", profile, "qos-doc-text")
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
FILES_APP = "dev.qos.file-explorer"
WORD_PROCESSOR = "dev.qos.word-processor"

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
# machine's own, and the trail ends `… › home › quinten`.
#
# It is still throwaway state — wiped and rebuilt per run, like the run
# directory — just somewhere that does not name the tooling.
#
# It cannot be `/home/<user>`, which is what would render perfectly:
# that is the real user's home and is not ours to write into. See the
# note in `stage_home`, and `docs/demo-todo.md` for the app-side fix
# that would make the path stop mattering.
HOMES_ROOT = "/var/tmp/qos/home"

# The photograph beat 8 drags into the document, by the name it wears on
# screen. Named once because four places have to agree about it: the
# staging that writes it, the assertion that it is there, the row the
# press has to land on, and the chip the compositor paints at the cursor
# while it crosses — that last one reads the name out of the `text/uri-list`
# the source app is offering, so what is on camera is the file's real name
# rather than anything this script told it to say.
PHOTO_NAME = "harbour.png"

MACHINE_APPS = os.path.expanduser("~/.local/share/qos-apps")
RUNTIME_BUNDLES = os.path.expanduser("~/.local/share/qos-runtime")
# A registry holding the word processor built from *this* tree. The
# machine's installed payload is a static binary from an earlier build
# and cannot exercise anything that has changed in `ui` since, so a run
# against it would be filming last month's app.
LIVE_APPS = os.path.join(REPO, "target", "qos-apps-live")

# The capability principal quinten grants in beat 3. An agent is not a
# peer: it is a subject in the permission model, and the grant is scoped
# to one app rather than to the workspace.
AGENT_SUBJECT = "agent:claude"
AGENT_SCOPE = f"app:{WORD_PROCESSOR}"

# What quinten types into the document, and what xaver's session is then
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

    #: Whether this session gossips its presence to peers on the network.
    #: On for the nine-beat film, whose whole subject is other people;
    #: worth turning off for a single-session take, because mdns finds
    #: every other qOS session on the machine — another agent's harness,
    #: a demo somebody left running — and paints a cursor and a name pill
    #: for it, over the document, on camera.
    peer_sync: bool = True

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
        # One language on screen. An app reads `LC_ALL`, then `LANGUAGE`,
        # then `LANG` (`apps/word-processor/src/i18n.rs:88`), bwrap does
        # not clear the environment, and the machine filming this is
        # German — so the shell chrome said "Home" and "Word Processor"
        # in English while the toolbar under it read "Neu / Tabelle /
        # Stile / Schriftart". Two languages in one frame reads as broken
        # software, and every assertion in this file passed while it did:
        # nothing here looks at a word on the screen.
        #
        # A real qOS install has the OS set this and cannot drift; a
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
            # all and persists into the real ~/.config/qos/state.json —
            # three demo sessions writing workspaces, owned by
            # identities nobody at this machine can administer, into
            # somebody's live desktop. `--out` is absolutised for the
            # same reason; this is the belt to that pair of braces.
            LC_ALL="en_US.UTF-8",
            LANG="en_US.UTF-8",
            QOS_STATE_PATH=os.path.join(self.dir, "config", "state.json"),
            QOS_USER_ID=self.user,
            QOS_PEER_KEY_PATH=os.path.join(self.dir, "keys", "peer-key"),
            QOS_FS_ACL_PATH=os.path.join(self.dir, "config", "fs-capabilities.json"),
            QOS_TEST_CONTROL=self.sock,
            NIRIOS_HEADLESS_SIZE=self.size,
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
                "info,shell::selection=debug,file_explorer::drag=debug"),
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
            # Makes a `ui` app name the record it checked out, on the
            # session log. Beat 7 depends on two apps announcing the
            # *same* address, and the only address two sessions can share
            # here is the untitled one: a record identified by a path
            # would be a different id on each side, because each session
            # has a home of its own. If that beat ever reports that
            # nobody answered, this line is what says whether the two
            # apps were even listening on the same channel.
            QUICK_COLLAB_DEBUG="1",
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
            self.key("k")
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

    def type_prose(self, text: str) -> None:
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

        Runs of unshifted characters still go through one `type` call, so
        this costs a round trip per capital and not per character.
        """
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
    "quinten": "hosts the room",
    "xaver": "joined by invite",
    "Claude": "admitted to the room",
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

        # How long each frame was the live state, before any editing.
        lived: list[float] = []
        for i, (t, _rel, _name) in enumerate(frames):
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
        digests = [self._digest(rel) for _t, rel, _n in frames]
        i = 0
        while i < len(frames):
            j = i + 1
            # A run has to stay on one session: a cut is a change even
            # when the two screens happen to be identical.
            while (j < len(frames)
                   and digests[j] == digests[i]
                   and digests[i] is not None
                   and frames[j][2] == frames[i][2]):
                j += 1
            runs.append((i, j - i))
            i = j

        for start, count in runs:
            _t, rel, name = frames[start]
            was_live = sum(lived[start:start + count])
            shown = min(was_live, max(tick, cap))
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


def frame_both_windows(s: Session, viewport: tuple[int, int] | None = None) -> None:
    """Put the whole strip in frame: even shares, and scrolled back.

    Two separate things go wrong, and only the first was being handled.

    A group takes about three quarters of the strip by default, which is
    right for reading one window and wrong for filming two — so every
    group is given an even share.

    The second is the strip's scroll offset. Opening a window in a new
    group scrolls the strip to bring *that* group up, and it does not
    come back when the content would fit whole: measured at 1920 wide,
    two even groups land at `-222 … 578` and `586 … 1386`, so the first
    window is cut by 222px on the left while 534px of empty backdrop
    sits on the right. Narrowing the groups does not help — the offset
    stays at -222 and only the windows get smaller — and scrolling the
    backdrop does not move it either.

    Focusing the leftmost group does move it: the same two windows then
    sit at `291 … 1091` and `1099 … 1899`, both whole, filling the strip
    beside the wheel disc. So that is the gesture — a click on the
    window you have just opened something next to, which is what a
    person does anyway.

    It is aimed at the tab pill rather than into the window's body: a
    click in the body reaches the client and selects whatever is under
    it, and a file row highlighted by the camera is the camera making
    the picture rather than the demo.
    """
    resp = s.set_group_width(500)
    if "err" in resp:
        note(f"{s.name}: could not even out the strip — "
             f"{json.dumps(resp['err'])[:120]}")
    s.settle(0.6)

    # The leftmost client tile, by where it is drawn rather than by the
    # order the tree lists it in.
    tiles = []
    for name in s.target_names():
        if not name.startswith("foreign-surface-"):
            continue
        rect = s.named_rect(name)
        if rect:
            tiles.append((rect[0], name, rect))
    if len(tiles) > 1:
        tiles.sort()
        _x, name, rect = tiles[0]
        x, y, w, _h = rect
        # The pill sits above the tile's top edge; 18px up lands on it
        # and stays clear of the client's own first row.
        s.click_at(max(4.0, x + min(90.0, w / 2.0)), max(4.0, y - 18.0))
        s.settle(0.8)
        note(f"{s.name}: focused the left group ({name}) so the strip "
             f"scrolls back and both windows are in frame")

    if viewport:
        clipped = windows_off_screen(s, viewport)
        if clipped:
            gap(f"{s.name}: a window is not wholly on screen — "
                + "; ".join(clipped))
        else:
            note(f"{s.name}: every window is wholly on screen")


def peer_sync_card_up(s: Session) -> bool:
    """Whether the peer-sync card is being painted over the middle of the
    screen.

    The compositor paints it whenever either field is set
    (`overlay_host.rs:9104`), so both have to be clear before it is gone —
    checking only the token left the connect prompt on screen.
    """
    ps = s.chrome()["peer_sync"]
    return ps["invite_token"] is not None or ps["connect_input"] is not None


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


def mint_invite(host: Session) -> str | None:
    host.run_palette_command("invite peer", expect_id="result:0")
    token = wait_for(lambda: host.chrome()["peer_sync"]["invite_token"], timeout=20)
    # The moment it has been read, and not one beat later: it is a modal
    # over the middle of the screen and it is showing a live token.
    dismiss_peer_sync_card(host, "invite")
    return token


def peer_id_in_token(token: str) -> str | None:
    """The peer id inside a session's own invite.

    Named for the token rather than for the peer, because a
    `peer_id_of(session)` already exists a few dozen lines up and reads
    the same id out of a session log. Two functions with one name is a
    collision Python resolves silently in favour of whichever is defined
    last — here that handed a `Session` to a base64 decoder, which
    raised, which the `except` turned into `None` for all three
    sessions, which read as "three sessions share one identity". The
    shell was fine.

    A token names the machine that issued it, so a session's own invite
    is also how that session says who it is. That is what lets one
    person hand another the identity they are about to be invited
    under, without anything new on the wire.
    """
    try:
        pad = "=" * (-len(token) % 4)
        blob = json.loads(base64.urlsafe_b64decode(token + pad).decode())
        return blob.get("owner_peer_id") or None
    except Exception:
        return None


def show_collaborators(s: Session) -> str:
    """Open the room's people from whatever affordance is on screen.

    The top bar's avatar stack is the affordance
    (`topbar_chrome.rs:1736` toggles `overlays.collaborators`), and it is
    the one a person reaches for. It is only *there* once somebody is in
    the room: with an empty roster the stack is `Rect::ZERO`
    (`topbar_chrome.rs:1611`), so it is not drawn and cannot be clicked —
    which is exactly the moment the host wants to invite the first
    person. Until that is fixed the panel is opened by its address for
    the first invite, and the route taken is named out loud, because
    "clicked the top bar" and "opened it by address" are different
    claims about what a viewer is being shown.
    """
    if s.named_rect("topbar_avatar_stack"):
        s.click_named("topbar_avatar_stack")
        s.settle(1.0)
        return "the avatar stack in the top bar"
    resp = s.invoke_action(AGENT_PROVIDER, "collaborators.open")
    if "err" in resp:
        note(f"{s.name}: collaborators.open refused — "
             f"{json.dumps(resp['err'])[:120]}")
        return "nothing - the panel could not be opened"
    s.settle(1.0)
    return "its address (the avatar stack is not drawn on an empty room)"


def close_collaborators(s: Session) -> None:
    if s.overlays().get("collaborators"):
        s.key("Escape")
        s.settle(0.4)


def mint_named_invite(host: Session, peer: str, name: str,
                      shots: str | None = None,
                      tag: str | None = None) -> str | None:
    """Invite a *named* peer, and get the token onto the clipboard.

    A plain `peer.invite` says nothing about who its token is for, and
    the host records only his own name at mint time — so naming resolved
    in one direction only: a guest saw `quinten` while quinten saw
    `peer@…`, and a cursor nobody can attribute to a person is the thing
    beat 4 exists to disprove.

    The copy is a **click on the card's own Copy button**, not a chord
    and not a read of chrome state. That matters twice over: it is the
    gesture a person makes, and what leaves here is what the button
    actually produced — a copy that truncated fails at the assertion
    below rather than being papered over by a token the harness already
    had in a variable.
    """
    resp = host.invoke_action(AGENT_PROVIDER, "peer.invite.named",
                              {"peer": peer, "name": name})
    if "err" in resp:
        note(f"peer.invite.named refused: {json.dumps(resp['err'])[:120]}")
        return None
    token = wait_for(lambda: host.chrome()["peer_sync"]["invite_token"],
                     timeout=20)
    if not token:
        return None
    # Long enough to read: this is the card the beat is about, and it is
    # the only place the token is ever visible.
    if RECORDER:
        RECORDER.dwell()
        RECORDER.dwell()
    if shots and tag:
        host.snapshot(os.path.join(shots, f"{tag}-invite-card.png"))

    step("he presses Copy on the card")
    clicked = host.click_named("peer_sync_copy")
    if "err" in clicked:
        gap("the card's Copy button was not addressable - "
            f"{json.dumps(clicked['err'])[:120]}")
        dismiss_peer_sync_card(host, "invite")
        return token
    copied = wait_for(lambda: host.clipboard_read(), timeout=10)
    claim("the token is on his clipboard, put there by the button",
          copied == token, True)
    if copied != token:
        note(f"the card shows {len(token)} characters; the clipboard holds "
             f"{len(copied or '')}")
    claim("and the card says so, where he is looking",
          "Copied" in (host.chrome()["peer_sync"].get("status") or ""), True)
    if RECORDER:
        RECORDER.dwell()
    if shots and tag:
        host.snapshot(os.path.join(shots, f"{tag}-copied.png"))
    dismiss_peer_sync_card(host, "invite")
    return copied or token


def introduce(guest: Session) -> str | None:
    """Have `guest` say who it is, by minting its own invite.

    Off camera on purpose: it is a card on the guest's screen carrying a
    token nobody redeems, and the beat it serves is the host's.
    """
    guest.run_palette_command("invite peer", expect_id="result:0")
    token = wait_for(lambda: guest.chrome()["peer_sync"]["invite_token"],
                     timeout=20)
    dismiss_peer_sync_card(guest, "invite")
    return peer_id_in_token(token or "")


def join_room(guest: Session, token: str, by_action_api: bool,
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

    `by_action_api` opens the prompt by invoking its stable address
    rather than by driving the palette.
    """
    guest.clipboard_write(token)
    if by_action_api:
        resp = guest.invoke_action(AGENT_PROVIDER, "peer.connect")
        if "err" in resp:
            raise RuntimeError(f"{guest.name}: peer.connect refused: {resp['err']}")
    else:
        guest.run_palette_command("connect to peer", expect_id="result:0")
    if not wait_for(lambda: guest.chrome()["peer_sync"]["connect_input"] is not None,
                    timeout=15):
        return False
    if RECORDER:
        RECORDER.dwell()

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
    if RECORDER:
        RECORDER.dwell()

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

    Decoding is delegated to `qos-doc-text`, which walks the object
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
    `… › quickdesktop › .claude › worktrees › yc-demo › target › …`. A
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
    """`qos-agent`, if it has been built.

    A separate process rather than a compositor: the whole point of it
    is that a participant can hold a document, a caret and a name in the
    room's history without holding a screen. Absent is a supported
    state — the beat says so and stops, rather than substituting
    something that looks similar.
    """
    for profile in ("demo", "release", "debug"):
        path = os.path.join(REPO, "target", profile, "qos-agent")
        if os.path.exists(path):
            return path
    return None


def run_the_agent(agent: Session, room: str, shots: str,
                  timeout: float = 120.0) -> bool | None:
    """Let the agent join the document and carry out one instruction.

    It connects to its own session's broker, which is how its edits
    reach the other two: the same libp2p republish every peer's edits
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
            return False
    if code != 0:
        with open(log_path) as fh:
            tail = fh.read()[-400:]
        note(f"qos-agent exited {code}: {tail}")
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


def beat_one(quinten: Session, shots: str) -> tuple[dict, dict | None]:
    banner("1.  an empty desktop, and Files opened on it")
    # The host's screen. A first boot really is empty here — one
    # workspace, no strips, no windows — so "empty" is read off the tree
    # rather than asserted about a backdrop.
    cut("quinten")

    ids = {s.name: peer_id_of(s) for s in SESSIONS}
    for s in SESSIONS:
        note(f"{s.name:8} pid {s.proc.pid:<8} user {s.user:<8} peer {ids[s.name]}")
    claim("three separate processes", len({s.proc.pid for s in SESSIONS}), 3)
    claim("three separate libp2p identities", len(set(ids.values())), 3)

    host_ws = quinten.active_workspace()
    note(f"quinten's room {host_ws['room']}  named {host_ws['name']!r}")
    claim("quinten owns his room", host_ws["owner"], quinten.user)
    claim("his desktop starts with no windows on it",
          len(quinten.tabs_here()), 0)
    quinten.snapshot(os.path.join(shots, "01-empty-desktop.png"))

    step("he opens Files")
    # By its own id through the applications provider — the registry that
    # feeds the palette. There is no on-screen affordance for the first
    # window; the palette is a chord, and this is the same row it offers.
    painted = quinten.launch(FILES_APP)
    claim("Files opened and painted a frame", painted is not None, True)
    if painted:
        note(f"surface {painted.get('surface_id')} at frame "
             f"{painted.get('frame_seq')}")
    tabs = quinten.tabs_here()
    claim("and it is the only window on his desktop", len(tabs), 1)
    quinten.snapshot(os.path.join(shots, "02-files.png"))
    # The surface travels with it: beat 8 drags a file *out of this
    # window*, and by then a second client is on screen announcing the
    # same wayland app-id. The surface the launch saw paint is the only
    # thing that tells the two apart.
    return host_ws, painted


def beat_two(quinten: Session, xaver: Session, host_ws: dict,
             shots: str) -> None:
    banner("2.  quinten invites xaver")
    cut("quinten")

    # Xaver says who he is first, off camera: his own invite names his
    # peer id, and that is what lets quinten invite *him* rather than
    # whoever redeems the token.
    xaver_peer = introduce(xaver)
    claim("xaver's peer id is known", bool(xaver_peer), True)

    step("he opens the room's people")
    # Through the chrome, at a pace a viewer can follow. This whole beat
    # used to pass in under a second because it was invoked as an action:
    # every claim held and the collaboration UI never appeared, which is
    # the one thing the beat exists to put on screen.
    cut("quinten")
    via = show_collaborators(quinten)
    note(f"opened from {via}")
    claim("the collaborators panel is up", quinten.overlays()["collaborators"],
          True)
    if RECORDER:
        RECORDER.dwell()
    quinten.snapshot(os.path.join(shots, "02a-collaborators-empty.png"))
    gap("the panel lists who is here and offers no way to invite anybody: "
        "there is no Invite row in it, so the invite itself is still "
        "raised by address rather than from the panel a person opened")
    close_collaborators(quinten)

    step("he invites xaver by name")
    token = mint_named_invite(quinten, xaver_peer or "", xaver.user,
                              shots, "02b")
    claim("quinten minted an invite token", bool(token), True)
    if not token:
        raise SystemExit("no invite token: the rest cannot run")
    note("the token is a signed blob carrying the room id, the owner's "
         "address, an expiry — and now the name the guest is invited "
         "under, so both machines can attribute a cursor to a person")

    # Redeeming is xaver's, and it is his screen that changes. The cut
    # lands on the prompt rather than on his empty desktop: a held shot
    # of a bare backdrop under a band reading "joined by invite" shows
    # nothing that supports what the band says.
    step("the token travels — by hand, off quinten's clipboard")
    cut("xaver")
    claim("xaver joined",
          join_room(xaver, token, by_action_api=False, shots=shots, tag="02c"),
          True)
    claim("xaver's own state holds quinten's room",
          xaver.active_workspace()["room"], host_ws["room"])
    claim("and his own copy names quinten as owner",
          xaver.active_workspace()["owner"], quinten.user)


def beat_three(quinten: Session, agent: Session, host_ws: dict,
               shots: str) -> None:
    banner("3.  quinten invites the agent")
    cut("quinten")

    agent_peer = introduce(agent)
    claim("the agent's peer id is known", bool(agent_peer), True)

    step("a second token — but an agent is not a peer")
    cut("quinten")
    token = mint_named_invite(quinten, agent_peer or "", agent.user,
                              shots, "03a")
    claim("quinten minted a second token", bool(token), True)
    if not token:
        raise SystemExit("no second token: the agent cannot join")

    # No cut. The agent has no screen worth filming and showing one
    # inverts the claim: it is a participant in somebody else's room, so
    # it appears as a row in a roster, a caret in a document and a name
    # in a history — never as a desktop of its own.
    claim("the agent joined the room", join_room(agent, token, by_action_api=True),
          True)
    claim("the agent's own state holds quinten's room",
          agent.active_workspace()["room"], host_ws["room"])
    # The connect *prompt* was opened by address; the token itself had to
    # be typed, because the control socket has no semantic join op.
    gap("joining has no semantic op — the invite token had to be typed "
        "(only the prompt was opened by address)")

    step("what the agent is allowed to do, before quinten says so")
    # The more interesting half, and the one that makes an agent a
    # different kind of participant: it is a subject in the permission
    # model, `agent:<id>`, and its authority is a grant on a scope. Asked
    # of the production gate, which answers about a verb on a scope.
    cut("quinten")

    def may_write(app: str) -> bool:
        answer = quinten.check_action(AGENT_SUBJECT, app, "SpliceText") \
            .get("answer", {}) or {}
        return bool(answer.get("permitted"))

    probe = quinten.check_action(AGENT_SUBJECT, WORD_PROCESSOR, "SpliceText")
    note(f"check_action({AGENT_SUBJECT}, {WORD_PROCESSOR}, SpliceText) -> "
         f"{json.dumps(probe.get('answer', probe))[:140]}")
    claim("the agent may not write to the word processor yet",
          may_write(WORD_PROCESSOR), False)

    step(f"quinten grants it write on {AGENT_SCOPE} — that app, and nothing else")
    granted = quinten.grant(AGENT_SUBJECT, AGENT_SCOPE, "write")
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
    # xaver would be.
    admitted = quinten.grant(f"person:{agent.user}", "workspace", "write")
    claim("quinten admits the agent's session as an editor",
          "err" not in admitted, True)
    if "err" in admitted:
        note(json.dumps(admitted["err"])[:200])


def beat_four(quinten: Session, xaver: Session, agent: Session,
              host_ws: dict, shots: str) -> None:
    banner("4.  both have joined — and each session can see the others")

    step("read quinten's room back out of the other two")
    # Neither of these is quinten's own state. Each guest independently
    # reports the room it now holds, so the camera sits on the guest
    # doing the reporting.
    cut("xaver")
    claim("xaver's own copy carries quinten's name for the room",
          xaver.active_workspace()["name"], host_ws["name"])
    # Read from the agent's session, shot on nobody's: it is asserted
    # here and *seen* in the roster on quinten's screen below.
    claim("the agent's own copy carries it too",
          agent.active_workspace()["name"], host_ws["name"])

    step("and the room's people, from the top bar")
    # Now there is a roster, so the affordance exists: the avatar stack
    # is drawn and clicking it opens the panel. This is also where the
    # names have to hold up — a roster listing `peer-2` and
    # `peer@11895642303397647149` beside `xaver` is three label styles
    # for three participants, and a room nobody can read.
    cut("quinten")
    via = show_collaborators(quinten)
    note(f"opened from {via}")
    claim("the collaborators panel is up on quinten's screen",
          quinten.overlays()["collaborators"], True)
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
    members: dict[str, list[str]] = {}
    for cap in (quinten.active_workspace().get("capabilities") or []):
        subject = json.dumps(cap.get("subject"))
        for name in (quinten.user, xaver.user, agent.user):
            if f'"{name}"' in subject or f":{name}" in subject:
                members.setdefault(name, []).append(str(cap.get("verb")))
    note(f"the room's own capability list names {sorted(members)}")
    claim("the agent is in the room the same way the people are — by "
          "holding a capability in it", agent.user in members, True)
    if agent.user in members:
        note(f"{agent.user} holds {sorted(set(members[agent.user]))} here, "
             f"and no cursor: an agent is a participant whose actions are "
             f"visible, not a mouse on somebody's screen")
    if RECORDER:
        RECORDER.dwell()
        RECORDER.dwell()
    quinten.snapshot(os.path.join(shots, "03a-roster.png"))
    close_collaborators(quinten)

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
    cut("quinten")
    quinten.pointer_move(*WHERE_Q)
    quinten.settle(0.6)
    cut("xaver")
    xaver.pointer_move(*WHERE_X)
    xaver.settle(0.6)

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
        written against `user == "quinten"` stopped matching anything at
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

    on_xaver = wait_for(lambda: seen_by(xaver, quinten.user), timeout=SYNC_TIMEOUT_S)
    claim("xaver's session paints quinten's cursor on an empty desktop",
          bool(on_xaver), True)
    if on_xaver:
        row = on_xaver[0]
        note(f"xaver's screen holds {json.dumps(row)}")
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
              row.get("name"), quinten.user)
        claim(f"and puts it where he actually is, not at the origin",
              at_the_origin(row), False)
        if at_the_origin(row):
            note(f"quinten's pointer is at {WHERE_Q} and his peer row on "
                 f"xaver's screen is at the origin — a cursor is being "
                 f"painted where he is not")
    xaver.snapshot(os.path.join(shots, "03-xaver-joined.png"))

    cut("quinten")
    on_quinten = wait_for(lambda: seen_by(quinten, xaver.user), timeout=SYNC_TIMEOUT_S)
    claim("and quinten's paints xaver's, the other way", bool(on_quinten), True)
    if on_quinten:
        claim("also at a real position", at_the_origin(on_quinten[0]), False)

    step("and they stay there without anyone moving")
    # The property that makes this beat filmable, asserted rather than
    # assumed. An edge-triggered cursor is published when it moves and
    # then forgotten, which looks identical to this one for a second and
    # then leaves an empty desktop behind — and means a peer arriving
    # later sees nobody until somebody jiggles a mouse. Nothing is moved
    # here on purpose: both pointers have been still since above.
    quinten.settle(3.0)
    still_there = seen_by(quinten, xaver.user)
    claim("xaver's cursor is still on quinten's screen 3s after it stopped",
          bool(still_there), True)
    if not still_there:
        note("a stopped pointer is no longer being restated, so a peer who "
             "joins after the last mouse movement arrives to an empty room")


def beat_five(quinten: Session, registry, shots: str) -> dict | None:
    banner("5.  \"word processor\" — a machine that does not have it")
    cut("quinten")

    installed = set(installed_ids(quinten.apps_dir))
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
    quinten.modifiers(logo=True)
    quinten.key("k")
    quinten.modifiers()
    quinten.settle(0.4)
    quinten.type_text("word processor")
    quinten.settle(1.2)
    pal = quinten.palette()
    rows = pal.get("rows") or []
    offered = next((r for r in rows if WORD_PROCESSOR in json.dumps(r)), None)
    claim("the palette offers an app this machine does not have",
          offered is not None, True)
    if offered:
        note(f"row: {offered.get('title')!r} — {offered.get('subtitle')!r}")
    quinten.snapshot(os.path.join(shots, "04-palette-offer.png"))

    step("he presses Enter — fetch, verify, unpack, run")
    before = {f.get("surface_id") for f in quinten.foreign_frames()}
    # The one number in this film a viewer might time with a stopwatch,
    # so the camera gets out of the way while it is being taken. The
    # poll below queues behind the camera too, which is why it loosens
    # as well.
    with easing(4.0):
        quinten.key("Enter")
        began = time.time()
        painted = None
        deadline = time.time() + LAUNCH_TIMEOUT_S * 2
        while time.time() < deadline:
            for f in quinten.foreign_frames():
                if f.get("surface_id") not in before and f.get("frame_seq", 0) > 0:
                    painted = f
                    break
            if painted:
                break
            time.sleep(0.25)
        took = time.time() - began
    claim("the word processor was fetched and opened", painted is not None, True)
    if painted is None:
        note(f"see {quinten.log_path}")
        return None
    note(f"{took:.1f}s from Enter to first frame — one machine, one LAN hop, "
         f"an unoptimised payload; not a number to quote")
    claim("and it is installed now, on the machine that lacked it",
          WORD_PROCESSOR in set(installed_ids(quinten.apps_dir)), True)
    # A group takes about three quarters of the strip by default, so the
    # second window opens mostly past the right edge — the strip is
    # working exactly as designed, and the app the beat is about is half
    # off screen while it does. Give every group an even share, so the
    # window this beat just opened is actually in the frame that shows it.
    frame_both_windows(quinten, VIEWPORT)
    quinten.snapshot(os.path.join(shots, "05-word-processor.png"))
    return painted


def beat_six(quinten: Session, xaver: Session, baseline: dict,
             shots: str) -> str | None:
    banner("6.  cut to xaver — the window is open for him too")

    # Both halves of the rule get asserted, because either alone is easy
    # to satisfy wrongly: the window must arrive, *and* he must not be
    # moved to it.
    #
    # The baseline is taken before beat 5 opens anything, not here.
    # Measuring "a tab quinten has that xaver lacks" at this point asks
    # the question after the answer: replication is quick, and a run
    # where the window had already crossed reported that there was
    # nothing to watch — the success and the failure both came out as
    # silence. What is watched instead is the tab that did not exist
    # when the baseline was taken, which is the window beat 5 opened.
    cut("xaver")
    before_view = baseline["view"]
    before_groups = baseline["groups"]
    q_tabs = set(quinten.tabs_here())
    fresh = [t for t in q_tabs if t not in baseline["tabs"]]
    note(f"quinten holds {len(q_tabs)} tab(s); xaver holds "
         f"{len(xaver.tabs_here())}; {len(fresh)} opened since the baseline")

    # Keep this beat on one of qOS's own apps. Those replicate through
    # the room document and are drawn from it on the far side, which is
    # what makes a real window appear there. A foreign Wayland surface
    # has no such description to replicate and degrades to a card
    # reading "coming soon" — true to what the system can do today, and
    # the wrong thing to put in a shot whose whole claim is that the
    # window opened over there.
    step("the tab quinten's word processor opened, read out of xaver's tree")
    if not fresh:
        gap("beat 5 opened no window on quinten's session, so there was "
            "nothing for this beat to watch cross")
        return None
    tid = sorted(fresh)[0]
    landed = wait_for(lambda: tid in xaver.tabs_here(), timeout=DOC_SYNC_TIMEOUT_S)
    claim("the window quinten opened is open for xaver too", bool(landed), True)
    if landed:
        arrived = xaver.tabs_here()[tid]
        note(f"tab {tid} in xaver's workspace, titled {arrived.get('title')!r}")
    else:
        note(f"tab {tid} had not replicated after {DOC_SYNC_TIMEOUT_S:.0f}s")

    step("and his own view did not move")
    after_view = xaver.view_state()
    claim("xaver's active workspace did not move",
          after_view["active"], before_view["active"])
    claim("xaver's scrolled-to strip did not move",
          after_view["active_strip"], before_view["active_strip"])
    claim("xaver's focus did not move",
          after_view["active_group"], before_view["active_group"])
    for g, was in before_groups.items():
        if g in xaver.groups_here():
            claim(f"the tab in front of him in group {g[:6]}… did not change",
                  xaver.groups_here()[g]["active_tab"], was["active_tab"])
    note("which windows exist is the workspace; which one you are looking "
         "at is you, and the second does not travel")
    xaver.snapshot(os.path.join(shots, "06-xaver-window.png"))
    return tid


def beat_seven(quinten: Session, xaver: Session, host_ws: dict,
               quinten_wp: dict | None, shots: str) -> None:
    banner("7.  quinten writes, and xaver's screen is asked what it holds")
    room = host_ws["room"]

    # If beat 5 never got a word processor onto quinten's machine there is
    # nothing here to type into, and typing anyway would send the text to
    # whatever happened to hold focus — which reads afterwards exactly
    # like a document that failed to cross. Say which of the two it was.
    if quinten_wp is None:
        gap("quinten has no word processor open (beat 5 did not put one "
            "there), so there was nothing for him to write in and nothing "
            "for this beat to watch cross")
        return

    step("xaver opens the word processor on his own machine")
    cut("xaver")
    # His machine runs its own copy and draws it locally. Nothing is
    # streamed; the two copies share one document.
    his = xaver.launch(WORD_PROCESSOR, search="word processor")
    claim("xaver's word processor painted", his is not None, True)
    if his is None:
        gap("xaver has no word processor open, so there is no far screen "
            "to read a document back off")
        return
    frame_both_windows(xaver)
    # Both apps have to have attached to their broker and announced their
    # address before either types, or the first keystrokes go out to
    # nobody and it reads as a transport failure.
    xaver.settle(6.0)

    step(f"quinten types {TYPED_TEXT!r}")
    cut("quinten")
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
    quinten.type_text(TYPED_TEXT)
    quinten.settle(1.5)

    def quinten_holds() -> bool:
        """Ask his own app whether the text is in its document."""
        reader = None
        try:
            reader = DocumentReader(quinten.collab_json_sock, room,
                                    record=None, handle="control")
            _doc, found = reader.await_document(TYPED_TEXT, 20.0)
            return bool(found)
        except OSError as e:
            note(f"quinten's own session could not be asked ({e})")
            return False
        finally:
            if reader is not None:
                reader.close()

    typed_here = quinten_holds()
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
        surface = (quinten_wp or {}).get("surface_id")
        stem = f"foreign-surface-{surface}"
        named = next((n for n in quinten.target_names()
                      if n == stem or n.startswith(stem + "[")), None)
        rect = quinten.named_rect(named) if named else None
        if not rect:
            others = [n for n in quinten.target_names()
                      if n.startswith("foreign-surface-")]
            gap(f"quinten's word processor (surface {surface}) has no "
                f"laid-out tile either, so there is nowhere to aim a caret. "
                f"Laid out right now: {others or 'no client tiles at all'}")
        else:
            bx, by, bw, bh = rect
            at = (bx + bw * 0.4, by + bh * 0.55)
            note(f"his document is {named} at {[round(v) for v in rect]}; "
                 f"clicking {[round(v) for v in at]}")
            quinten.click_at(*at)
            quinten.settle(1.2)
            quinten.type_text(TYPED_TEXT)
            quinten.settle(1.0)
            typed_here = quinten_holds()
    quinten.snapshot(os.path.join(shots, "07-quinten-typed.png"))

    step("first, that he typed into the document and not at the chrome")
    # The control, and the reason this beat can tell its two failures
    # apart. If the text never reaches xaver, that is either a document
    # which did not cross or a document which never had the text in it,
    # and both end as an empty page on the far screen.
    claim("quinten's own document holds what he typed", typed_here, True)
    if not typed_here:
        note("the keystrokes did not reach his document at all, so nothing "
             "downstream of this is about the network")

    step("and xaver's session is asked what it now holds")
    cut("xaver")
    # What is mechanically readable from here is presence: the caret and
    # pointer of the peer who is typing, attributed to his user id, off
    # the same context the overlay paints from.
    def quinten_present():
        # Either field: the identity is `peer@<handle>` and the name is
        # what the room calls it. See `seen_by` in beat 4.
        rows = (xaver.peers() or {}).get("pointers") or []
        return [r for r in rows
                if quinten.user in (r.get("name"), r.get("user"))] or None

    seen = wait_for(quinten_present, timeout=SYNC_TIMEOUT_S)
    if seen:
        claim("xaver's screen carries quinten while he is typing",
              bool(seen), True)
    else:
        gap("quinten's presence did not reach xaver's session")

    step("and his word processor is asked for the document it is holding")
    # The beat itself, and the one claim whose subject is inside a
    # client. Asked of xaver's *app*, in the app's own protocol: a late
    # joiner on his session's broker announces the word processor's
    # address and sends the `Arrived` every toolkit answers with its
    # whole document. What comes back was produced by his running app
    # out of the document it holds, so the answer is about his screen
    # and not about the wire.
    #
    # The camera stays on xaver for the whole wait, so if the document
    # converges the arrival is on film rather than described afterwards.
    reader = None
    doc, carried = None, False
    try:
        reader = DocumentReader(xaver.collab_json_sock, room, record=None)
        doc, carried = reader.await_document(TYPED_TEXT, DOC_SYNC_TIMEOUT_S)
    except OSError as e:
        gap(f"xaver's session has no JSON broker to ask ({e}) — "
            f"looked at {xaver.collab_json_sock}")
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
        claim("xaver's own word processor holds what quinten typed",
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
                 f"what quinten wrote")
    else:
        gap(f"no app in xaver's session answered the late-join handshake "
            f"within {DOC_SYNC_TIMEOUT_S:.0f}s, so his document was never "
            f"read back — this run establishes nothing about beat 7 in "
            f"either direction. See {xaver.log_path}")
    xaver.settle(2.0)
    xaver.snapshot(os.path.join(shots, "08-xaver-received.png"))


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


def press_the_photo_row(quinten: Session, log: LogTail, tile: tuple,
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
        quinten.pointer_move(x, at_y)
        quinten.settle(0.15)
        log.read()
        quinten.pointer_button(True)
        # The press has to cross a process boundary, be dispatched by
        # the app's event loop and be written to a log this then reads.
        # 0.5s is the difference between "the app did not report it" and
        # "the app had not reported it yet".
        quinten.settle(0.5)
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
            quinten.pointer_button(False)
            quinten.settle(0.2)
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
            quinten.pointer_button(False)
            quinten.settle(0.2)
            break
        try:
            hit_index = [n.lower() for n in listing].index(
                os.path.basename(path).lower())
        except ValueError:
            quinten.pointer_button(False)
            quinten.settle(0.2)
            continue
        offset = y - client_y          # screen y  =  client y + offset
        aimed = (client_y + (target_index - hit_index) * LIST_ROW_H + offset)
        note(f"the press landed on {os.path.basename(path)} "
             f"(row {hit_index}); {photo_name} is row {target_index}, "
             f"{abs(target_index - hit_index)} rows "
             f"{'down' if target_index > hit_index else 'up'} at "
             f"y={aimed:.0f}")
        quinten.pointer_button(False)
        quinten.settle(0.3)
        again = press(aimed)
        if again and os.path.basename(again[0]).lower() == want:
            note(f"pressed on {os.path.basename(again[0])} at y={aimed:.0f}")
            return (x, aimed)
        quinten.pointer_button(False)
        quinten.settle(0.2)
        break
    return None


def beat_eight(quinten: Session, files: dict | None, wp: dict | None,
               shots: str) -> None:
    banner("8.  a photograph dragged out of Files and into the document")
    cut("quinten")

    # What makes this beat worth filming is not that a picture appears in
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

    photo = os.path.join(quinten.home, PHOTO_NAME)
    claim("there is a photograph in his home to drag",
          os.path.exists(photo), True)
    if not os.path.exists(photo):
        return
    payload = open(photo, "rb").read()

    # Both windows whole, before anything is aimed at either of them. A
    # drag that starts or ends off the edge of the screen is not a drag
    # anybody can see.
    unpainted = wait_until_painted(quinten)
    if unpainted:
        gap(f"the shell is not painting {unpainted} yet, so this would be "
            f"filmed against an empty desktop")
    frame_both_windows(quinten, VIEWPORT)
    quinten.settle(0.5)
    tiles = painted_tiles(quinten)
    if VIEWPORT:
        clipped = windows_off_screen(quinten, VIEWPORT)
        claim("both windows are wholly on screen before the gesture",
              clipped, [])
        if clipped:
            gap("a window is cut off, so the gesture would cross an edge "
                "the camera cannot show — " + "; ".join(clipped))
            quinten.snapshot(os.path.join(shots, "09-not-in-frame.png"))
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
    claim("Files is on screen to drag out of", files_tile is not None, True)
    claim("the word processor is on screen to drop into",
          wp_tile is not None, True)
    if not files_tile or not wp_tile:
        gap("both windows have to be on screen at once for this gesture; "
            f"the session is painting {[t['app_id'] for t in tiles.values()]}")
        return
    note(f"Files at {[round(v) for v in files_tile]}, "
         f"the document at {[round(v) for v in wp_tile]}")
    quinten.snapshot(os.path.join(shots, "09-before-drag.png"))

    log = LogTail(quinten.log_path)
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
    quinten.click_at(wx0 + ww0 * 0.30, wy0 + wh0 * 0.30)
    quinten.settle(0.8)
    quinten.key_down("Control")
    quinten.key("End")
    quinten.key_up("Control")
    quinten.key("Enter")
    quinten.settle(0.6)

    step(f"he presses on {PHOTO_NAME}")
    # A click first, so the press that starts the drag is not the one
    # that focuses the window. The first click on an unfocused client is
    # spent on the focus change and never reaches the app's own widgets —
    # measured, and the cause of a drag that produced nothing at all.
    fx, fy, fw, fh = files_tile
    quinten.click_at(fx + fw * 0.45, fy + fh * 0.16)
    quinten.settle(0.8)

    listing = expected_listing(quinten.home)
    note(f"his home lists as {listing}")
    held = press_the_photo_row(quinten, log, files_tile, PHOTO_NAME, listing)
    claim(f"the press landed on {PHOTO_NAME}'s own row", held is not None, True)
    if held is None:
        gap(f"no press landed on {PHOTO_NAME}: the file rows belong to a "
            f"client and are not addressable by name from the compositor, "
            f"so this is aimed at pixels and the app did not report a row "
            f"under any of them. See {quinten.log_path}")
        quinten.snapshot(os.path.join(shots, "09-missed-the-row.png"))
        return
    hx, hy = held

    step("and drags it across the desktop")
    # Past the app's own drag threshold first, slowly, while the button
    # is still down. That is the moment Files opens a `wl_data_source`
    # and hands the gesture to the compositor; a drag that teleports to
    # its destination never gives either process a frame in which to do
    # it, and nothing is ever picked up.
    for step_px in (4, 10, 18, 30):
        quinten.pointer_move(hx + step_px, hy - step_px * 0.4)
        quinten.settle(0.12)
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
        quinten.pointer_move(x0 + (drop[0] - x0) * t,
                             y0 + (drop[1] - y0) * t)
        quinten.settle(0.05)
        if i == frames // 2:
            # Mid-flight, over open desktop between the two windows:
            # this is the frame that shows the drag is the OS's and not
            # either app's.
            quinten.snapshot(os.path.join(shots, "10-carrying.png"))
    quinten.settle(0.4)
    quinten.snapshot(os.path.join(shots, "11-over-the-page.png"))

    step("and drops it on the page")
    log.read()
    quinten.pointer_button(False)

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
    quinten.settle(2.0)
    quinten.snapshot(os.path.join(shots, "12-image-in-the-document.png"))

    if not landed:
        gap(f"nothing the size of the photograph reached the document "
            f"within 25s of the drop. See {quinten.log_path}")
        return

    step("and it is in the document, not merely on the screen")
    # Asked of the app, after the camera has moved on: a late joiner on
    # this session's broker gets the whole document back, and the
    # photograph's own bytes are either in it or they are not.
    room = quinten.active_workspace()["room"]
    carried, size_bytes = image_is_in_the_document(quinten, room, payload)
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


def beat_nine(quinten: Session, xaver: Session, agent: Session,
              host_ws: dict, quinten_wp: dict | None, shots: str) -> None:
    banner("9.  the agent acts, and the room shows who did it")
    # The camera stays on the people. An agent is a participant in
    # somebody else's room, so what it does is shown the way any other
    # collaborator's actions are shown — arriving on their screens — and
    # never as a desktop of its own. Everything it is asked below is
    # asked over its control socket and printed to the transcript; none
    # of it is filmed, because there is nothing there to film.
    cut("quinten")

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

    # What an agent cannot do here, said plainly rather than shot around.
    # The action model reaches the shell's own text tiles (`text.insert`
    # writes into a focused Notes or Editor tab) and stops at the edge of
    # a packaged app: the word processor is a foreign Wayland client, its
    # widgets are not addressable, and no exported action writes into its
    # document. The mechanism that *can* is `qos-agent`, which joins the
    # document through the collaboration broker and carries out an
    # instruction written to it — a participant with a caret and no
    # window. That is the beat this one should become.
    gap("the agent cannot change the shared document by address: no "
        "exported action reaches a packaged app's document, so what it "
        "does here opens a window rather than writing in the file the "
        "other two are looking at (see apps/agent, which does)")

    # A terminal used to be opened here, because opening a window was
    # the only visible thing an agent could do by address. It ended the
    # film on a `bash` prompt in a fourth window — a developer machine,
    # in the last shot a viewer keeps, arguing against the strip the
    # letter points at. What follows is the beat that was wanted
    # instead: the agent changes the document the other two are looking
    # at, and it is read back off somebody else's screen.
    step("quinten writes a list, and asks Claude for a table")
    cut("quinten")
    if not type_into_the_document(quinten, quinten_wp, AGENT_BULLETS):
        gap("nothing could be typed into quinten's document, so there is "
            "no list for the agent to act on")
        return
    quinten.settle(1.0)
    if RECORDER:
        RECORDER.dwell()
    quinten.type_prose(AGENT_INSTRUCTION)
    quinten.settle(1.5)
    quinten.snapshot(os.path.join(shots, "11-instruction.png"))
    note(f"the instruction is a line in the document: {AGENT_INSTRUCTION!r}")

    step("the agent reads it and carries it out — with no window at all")
    ran = run_the_agent(agent, host_ws["room"], shots)
    if ran is None:
        gap("qos-agent is not built, so the agent could not act on the "
            "document (cargo build -p agent --bin qos-agent)")
        return
    claim("the agent carried out the instruction", ran, True)
    claim("and no synthetic input reached its session to do it",
          agent.input_ops, baseline)

    step("and xaver's own word processor is asked what it holds now")
    # The payoff, read off the screen of somebody who did not make the
    # change. The assertion is that the *content* survived into the
    # table: a table with the rows empty is what this beat produced
    # before, and it is indistinguishable from success in a transcript
    # that only asks whether a table exists.
    cut("xaver")
    reader = None
    try:
        reader = DocumentReader(xaver.collab_json_sock, host_ws["room"],
                                record=None)
        doc, carried = reader.await_document(AGENT_BULLET_PROOF,
                                             DOC_SYNC_TIMEOUT_S)
    except OSError as e:
        gap(f"xaver's session has no JSON broker to ask ({e})")
        doc, carried = None, False
    finally:
        if reader is not None:
            reader.close()
    if doc is not None:
        claim("xaver's document carries the rows the agent made, with the "
              "text still in them", carried, True)
    else:
        gap("xaver's app never answered, so what his screen holds was not "
            "established")
    # The last shot of the film, so it is framed like one: even shares,
    # the strip scrolled back, and every window whole. An accumulation
    # of everything the run happened to open is the picture that argues
    # against the layout this is meant to show off.
    frame_both_windows(xaver, VIEWPORT)
    if RECORDER:
        RECORDER.dwell()
        RECORDER.dwell()
    xaver.snapshot(os.path.join(shots, "12-xaver-agent-table.png"))


def run(quinten: Session, xaver: Session, agent: Session, shots: str,
        registry) -> None:
    host_ws, files = beat_one(quinten, shots)
    beat_two(quinten, xaver, host_ws, shots)
    beat_three(quinten, agent, host_ws, shots)
    beat_four(quinten, xaver, agent, host_ws, shots)
    # Everything xaver holds and is looking at *before* the word
    # processor exists anywhere. Beat 6 measures against this rather than
    # against the state it finds when it starts, so "the window crossed
    # before we looked" reads as the success it is.
    baseline = {
        "tabs": set(xaver.tabs_here()),
        "view": xaver.view_state(),
        "groups": xaver.groups_here(),
    }
    wp = beat_five(quinten, registry, shots)
    beat_six(quinten, xaver, baseline, shots)
    beat_seven(quinten, xaver, host_ws, wp, shots)
    beat_eight(quinten, files, wp, shots)
    beat_nine(quinten, xaver, agent, host_ws, wp, shots)




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
            print("\n  cargo build --profile demo -p shell --bin shell "
                  "--bin qos-control")
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
    for short, name in zip("qxa", ("quinten", "xaver", "Claude")):
        run_dir = os.path.join(args.out, name)
        apps_dir = os.path.join(run_dir, "qos-apps")
        # Quinten is the machine that does not have it yet. Everyone
        # else does, so xaver can open his own copy in beat 7.
        compose_registry(apps_dir, entries,
                         drop=(WORD_PROCESSOR,) if name == "quinten" else ())
        sessions.append(Session(
            name, name, run_dir,
            os.path.join(runtime, f"qos-yc-{short}-{pid}.sock"),
            args.shell, size,
            # A private runtime dir per session, standing in for the
            # per-user one logind would give each real login.
            os.path.join(runtime, f"qyc-{short}{pid}"),
            stage_home(os.path.join(HOMES_ROOT, name)),
            apps_dir, tools))
    quinten, xaver, agent = sessions
    global SESSIONS, VIEWPORT
    SESSIONS = sessions
    VIEWPORT = tuple(int(v) for v in size.lower().split("x"))

    print(f"compositor:    {args.shell}")
    print(f"run directory: {args.out}")
    print(f"apps:          {len(entries)} installed for xaver and the agent, "
          f"{len(entries) - (1 if WORD_PROCESSOR in entries else 0)} for quinten")
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
            RECORDER.start(quinten.name)
        # The registry quinten fetches from in beat 5. Its config dir is
        # his store's, which is how the catalogue in his session finds it.
        with Registry(
            os.path.join(REPO, "target", "debug", "examples",
                         "serve-test-registry"),
            os.path.join(LIVE_APPS, "payloads", WORD_PROCESSOR, "bin",
                         "word-processor"),
            os.path.join(quinten.dir, "store-config"),
            os.path.join(args.out, "registry.log"),
        ) as registry:
            if registry is None:
                print("no local registry: beat 5 will report the fetch as "
                      "unfilmed rather than skip past it")
            else:
                print(f"registry:      {registry.base_url}")
            run(quinten, xaver, agent, shots, registry)
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
