#!/usr/bin/env python3
"""One person, two windows: nobody's own name floats over their document.

Every other presence proof here puts two *people* in a room and checks
that each sees the other. This asks the opposite question, and it is the
one that was wrong: what does a person see when the only participant is
themselves?

Two apps one person has open are two connections and **one
participant** — the broker says so outright and resolves both to a
single identity before it stamps their presence. It then hands each
app's frames to the rest of the room, and the compositor's overlay is
in that room too. So the desktop drew a cursor pill labelled with the
owner's own name, on top of the owner's own window, in every frame.
Turning peer gossip off did not remove it: the two ends were never
peers on a network, they were one person's own two processes.

The roster has refused this for as long as it has existed — the row
builder drops the local handle before it makes a row, because a host
self-names when it mints a room and must not read that name back as a
peer. This proves the same rule now holds on the path that paints.

What it produces is a screenshot. Look at it. No pill anywhere may
carry this session's own name.

    scripts/headless-examples/no-pill-for-yourself-proof.py

    cargo build -p shell -p tilo-launcher -p word-processor -p file-explorer

The related check is `peers-cross-both-ways.py`, which puts two people in a room
and requires each to keep seeing the other's caret and pointer, named
and coloured. A guard that silences your own pill by silencing
everybody passes this file and fails that one.

Exits non-zero if a claim fails.
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

REPO = pathlib.Path(__file__).resolve().parents[2]
SHELL = REPO / "target" / "debug" / "shell"
APPLICATIONS = "applications"

# Two *different* apps rather than one opened twice: the point is the
# participant, not the process or the package. A word processor and a
# file manager side by side are still one person, and if the rule were
# keyed on the app id instead this pair would slip past it.
APPS = ("so.tilo.word-processor", "so.tilo.file-explorer")

# The name this session runs under. Set explicitly so the assertion has
# a literal to look for rather than having to guess what the machine
# calls its owner.
WHO = "quinten"

failures: list[str] = []


def check(label: str, ok: bool, detail: str = "") -> bool:
    print(f"  {'PASS' if ok else 'FAIL'}  {label}" + (f" — {detail}" if detail else ""))
    if not ok:
        failures.append(label)
    return bool(ok)


def note(text: str) -> None:
    print(f"        {text}")


def wait_for(pred, timeout=40.0, poll=0.4):
    end = time.time() + timeout
    while time.time() < end:
        try:
            got = pred()
        except Exception:
            got = None
        if got:
            return got
        time.sleep(poll)
    return None


class Session:
    """One real compositor, isolated from the machine in everything but
    the two stores an app launch cannot do without."""

    def __init__(self, name: str, root: str, apps_dir: str | None = None):
        self.name = name
        self.dir = os.path.join(root, name)
        os.makedirs(self.dir, exist_ok=True)
        self.socket = os.path.join(self.dir, "control.sock")
        runtime = os.path.join(self.dir, "run")
        os.makedirs(runtime, mode=0o700, exist_ok=True)
        home = os.path.expanduser("~")
        env = {
            **os.environ,
            "TILO_TEST_CONTROL": self.socket,
            "TILO_STATE_PATH": os.path.join(self.dir, "config", "state.json"),
            "XDG_DATA_HOME": os.path.join(self.dir, "data"),
            "XDG_CACHE_HOME": os.path.join(self.dir, "cache"),
            "XDG_STATE_HOME": os.path.join(self.dir, "state"),
            "XDG_CONFIG_HOME": os.path.join(self.dir, "config"),
            "XDG_RUNTIME_DIR": runtime,
            "TILO_USER_ID": WHO,
            "TILO_APPS_DATA_DIR": apps_dir or os.path.join(home, ".local/share/tilo-apps"),
            "TILO_RUNTIME_BUNDLE_DIR": os.path.join(home, ".local/share/tilo-runtime"),
            "TILO_PEER_KEY_PATH": os.path.join(self.dir, "keys", "peer-key"),
            "TILO_HEADLESS_SIZE": "1280x800",
            "LC_ALL": "en_US.UTF-8",
            "LANG": "en_US.UTF-8",
            # Capture the toolkit's structured state/presence events. The
            # self-filter event is this rig's witness that presence crossed.
            "RUST_LOG": "tilo.state=debug",
        }
        env.pop("DISPLAY", None)
        env.pop("WAYLAND_DISPLAY", None)
        self.log_path = os.path.join(self.dir, "shell.log")
        self.log = open(self.log_path, "w")
        self.proc = subprocess.Popen(
            [str(SHELL), "--backend", "headless", "--enable-peer-sync"],
            env=env, stdout=self.log, stderr=subprocess.STDOUT,
            preexec_fn=die_with_parent(),
        )

    def wait_up(self, timeout=120.0) -> bool:
        end = time.time() + timeout
        while time.time() < end:
            if os.path.exists(self.socket):
                time.sleep(1.0)
                return True
            if self.proc.poll() is not None:
                return False
            time.sleep(0.3)
        return False

    def call(self, payload: dict, timeout: float = 60.0) -> dict:
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(timeout)
        s.connect(self.socket)
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

    def state(self) -> dict:
        return self.call({"op": "state_dump"})["state"]

    def chrome(self) -> dict:
        return self.state()["chrome"]

    def tree(self) -> dict:
        return self.state()["workspace_tree"]

    def active_room(self):
        tree = self.tree()
        for w in tree.get("workspaces") or []:
            if w.get("id") == tree.get("active"):
                return w.get("room")
        return None

    def invoke(self, ident: str, provider: str = "actions") -> dict:
        return self.call({"op": "actions_invoke", "provider": provider, "id": ident})

    def peers(self) -> dict:
        return self.state().get("peers") or {}

    def pointers(self) -> list:
        return self.peers().get("pointers") or []

    def roster(self) -> list:
        return self.peers().get("roster") or []

    def frames(self) -> list[dict]:
        r = self.call({"op": "foreign_frames"})
        if "foreign_frames" in r:
            return r["foreign_frames"].get("frames", [])
        return r.get("answer", r.get("frames", [])) or []

    def move_to(self, x: float, y: float):
        self.call({"op": "pointer_move", "x": x, "y": y})

    def snapshot(self, path: str) -> bool:
        self.call({"op": "snapshot", "path": path})
        for _ in range(40):
            if os.path.exists(path) and os.path.getsize(path) > 0:
                return True
            time.sleep(0.25)
        return False

    def stop(self):
        for closer in (self.proc.terminate, self.proc.kill):
            try:
                closer()
                self.proc.wait(timeout=8)
                break
            except Exception:
                continue
        try:
            self.log.close()
        except Exception:
            pass


def launch_and_paint(session: Session, app: str, timeout: float = 120.0) -> dict | None:
    """Open `app` and wait until it has actually taken a frame.

    A laid-out rect arrives seconds before the shell composites the
    tile, and in that window every check passes while a snapshot is an
    empty desktop. The frame counter is the ground truth: only a
    surface that was not there before this launch, and that has drawn
    at least once, is this app on screen.
    """
    before = {f.get("surface_id") for f in session.frames()}
    resp = session.call({"op": "actions_invoke", "provider": APPLICATIONS, "id": app})
    if "err" in resp:
        note(f"invoke refused for {app} — {json.dumps(resp['err'])}")
        return None
    end = time.time() + timeout
    while time.time() < end:
        for f in session.frames():
            if f.get("surface_id") not in before and f.get("frame_seq", 0) > 0:
                return f
        time.sleep(1.0)
    return None


def presence_traffic(session: Session) -> list[str]:
    """Lines proving one app received the other's presence.

    A `ui` app logs this when a participant arrives on a scope it is
    watching, and it logs it *because* the arrival still has to be
    answered with the document — the half of the frame that must keep
    working while the painted half stops. So the line is evidence the
    rig delivered the very frame that used to become a pill, and it
    cannot be produced by a session that published nothing.
    """
    try:
        text = open(session.log_path, errors="replace").read()
    except OSError:
        return []
    return [ln for ln in text.splitlines()
            if "recv Arrived" in ln or "replying full state" in ln]


def rule_fired(session: Session) -> list[str]:
    """Lines where an app recognised an arrival as its own participant.

    The positive half of the claim. Absence of a pill is also what a
    session that was sent nothing looks like, and the first version of
    this file passed against a binary carrying the bug for exactly that
    reason — the pill it missed was drawn by the app, inside its own
    window, where the compositor's presence store cannot see it.
    """
    try:
        text = open(session.log_path, errors="replace").read()
    except OSError:
        return []
    return [ln for ln in text.splitlines() if "own participant" in ln]


def mentions_self(rows: list) -> list:
    """Presence rows that name this session's own participant.

    Both fields, because they answer different questions and either one
    on a pill is the bug: `user` is the identity the broker stamped,
    `name` is what the room calls it — and it is the name that gets
    painted.
    """
    return [r for r in rows
            if r.get("user") == WHO or r.get("name") == WHO]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--keep", action="store_true", help="leave the session and files behind")
    ap.add_argument("--out", default=None, help="where to write the screenshot")
    ap.add_argument("--apps", default=None, help="override TILO_APPS_DATA_DIR")
    args = ap.parse_args()

    if not SHELL.exists():
        print(f"missing {SHELL} — cargo build -p shell", file=sys.stderr)
        return 2

    out = os.path.abspath(args.out or tempfile.mkdtemp(prefix="no-self-pill-"))
    os.makedirs(out, exist_ok=True)
    root = tempfile.mkdtemp(prefix="qs-")
    session: Session | None = None
    try:
        print(f"\n[1] one session, named {WHO!r}")
        session = Session("quinten", root, args.apps)
        if not check("the session came up", session.wait_up()):
            note(f"see {session.log_path}")
            return 1

        print("\n[2] a room to be alone in")
        # Presence is scoped to a room and an unminted workspace has
        # none: an app in one publishes nothing at all, which is a
        # desktop where this bug cannot appear and a test that would
        # pass without ever reaching the thing it is aiming at.
        #
        # Minting is also what writes this machine's own name into the
        # room — the host names itself when it mints, and that name is
        # what the pill later reads.
        session.invoke("peer.invite")
        token = wait_for(lambda: session.chrome()["peer_sync"]["invite_token"], timeout=30)
        if not check("the workspace has a room", bool(token)):
            note(f"no room means no presence at all; see {session.log_path}")
            return 1
        session.call({"op": "key", "key": "Escape"})
        room = session.active_room()
        note(f"room {room}")

        print("\n[3] two apps, one person")
        for app in APPS:
            frame = launch_and_paint(session, app)
            if not check(f"{app.rsplit('.', 1)[-1]} painted", bool(frame),
                         f"surface {frame.get('surface_id')}" if frame else ""):
                note(f"see {session.log_path}")
                return 1

        # Both apps announce and publish under one identity, and the
        # overlay subscribes to the same room. Drift the pointer across
        # both tiles so pointer presence has actually been published
        # rather than merely being possible — a rule that holds only
        # because nothing was ever sent is not a rule.
        for x, y in ((320.0, 400.0), (360.0, 420.0), (900.0, 400.0), (940.0, 420.0)):
            session.move_to(x, y)
            time.sleep(1.5)
        time.sleep(3.0)

        print("\n[4] presence was in fact published")
        # The witness this rig cannot do without. Once the rule holds,
        # a session alone in a room paints nobody — and an empty
        # presence store is exactly what a rig that never delivered a
        # frame also produces. Passing on that would be a test that
        # cannot tell "fixed" from "never looked", which is how the
        # first version of this file passed against a binary carrying
        # the bug.
        traffic = presence_traffic(session)
        check("the session's apps published presence into the room",
              bool(traffic), (traffic[-1][:160] if traffic else
                              "nothing in the log — the rig proved nothing"))

        fired = rule_fired(session)
        check("an app recognised the arrival as its own participant",
              bool(fired), (fired[-1][:160] if fired else
                            "the rule never fired — an empty screen proves nothing"))

        print("\n[5] what the desktop is painting")
        rows = session.pointers()
        note(f"presence rows: {json.dumps(rows)[:400]}")
        mine = mentions_self(rows)
        check("no cursor pill carries this session's own name",
              not mine, json.dumps(mine)[:300] if mine else f"{len(rows)} row(s), none self")

        listed = [r for r in session.roster() if r.get("label") == WHO]
        check("the roster does not list the owner as a collaborator",
              not listed, json.dumps(listed)[:200] if listed else "")

        print("\n[6] the pixels")
        shot = os.path.join(out, "one-person-two-windows.png")
        check("screenshot written", session.snapshot(shot), shot)
        note("look at it: no pill anywhere may read " + WHO)

    finally:
        if session and not args.keep:
            session.stop()
        if args.keep and session:
            note(f"session left running; logs under {session.dir}")

    print()
    if failures:
        print(f"FAILED: {len(failures)} — " + "; ".join(failures))
        return 1
    print("all claims held — now look at the screenshot in " + out)
    print("then run peers-cross-both-ways.py: this fix is only correct if")
    print("two real people in a room still see each other.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
