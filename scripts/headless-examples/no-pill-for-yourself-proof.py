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

    cargo build -p shell -p qos-launcher -p word-processor -p file-explorer

The control that keeps this fix honest is a separate script, and it is
not optional: `peers-cross-both-ways.py` puts two real people in a room
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
APPS = ("dev.qos.word-processor", "dev.qos.file-explorer")

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
            "QOS_TEST_CONTROL": self.socket,
            "QOS_STATE_PATH": os.path.join(self.dir, "config", "state.json"),
            "XDG_DATA_HOME": os.path.join(self.dir, "data"),
            "XDG_CACHE_HOME": os.path.join(self.dir, "cache"),
            "XDG_STATE_HOME": os.path.join(self.dir, "state"),
            "XDG_CONFIG_HOME": os.path.join(self.dir, "config"),
            "XDG_RUNTIME_DIR": runtime,
            "QOS_USER_ID": WHO,
            "QOS_APPS_DATA_DIR": apps_dir or os.path.join(home, ".local/share/qos-apps"),
            "QOS_RUNTIME_BUNDLE_DIR": os.path.join(home, ".local/share/qos-runtime"),
            "QOS_PEER_KEY_PATH": os.path.join(self.dir, "keys", "peer-key"),
            "NIRIOS_HEADLESS_SIZE": "1280x800",
            "LC_ALL": "en_US.UTF-8",
            "LANG": "en_US.UTF-8",
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
    ap.add_argument("--apps", default=None, help="override QOS_APPS_DATA_DIR")
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

        print("\n[2] two apps, one person")
        for app in APPS:
            frame = launch_and_paint(session, app)
            if not check(f"{app.rsplit('.', 1)[-1]} painted", bool(frame),
                         f"surface {frame.get('surface_id')}" if frame else ""):
                note(f"see {session.log_path}")
                return 1

        # Both apps announce and publish under one identity, and the
        # overlay is in the same room. Give the fan-out a moment to
        # deliver, and move the pointer so that any pointer-driven
        # presence has actually happened rather than merely being
        # possible — a rule that only holds because nothing was
        # published is not a rule.
        session.move_to(640.0, 400.0)
        time.sleep(3.0)
        session.move_to(700.0, 430.0)
        time.sleep(3.0)

        print("\n[3] what the desktop is painting")
        rows = session.pointers()
        note(f"presence rows: {json.dumps(rows)[:400]}")
        mine = mentions_self(rows)
        check("no cursor pill carries this session's own name",
              not mine, json.dumps(mine)[:300] if mine else f"{len(rows)} row(s), none self")

        listed = [r for r in session.roster() if r.get("label") == WHO]
        check("the roster does not list the owner as a collaborator",
              not listed, json.dumps(listed)[:200] if listed else "")

        print("\n[4] the pixels")
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
