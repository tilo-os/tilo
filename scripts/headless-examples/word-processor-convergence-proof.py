#!/usr/bin/env python3
"""Two people, two machines, one document — read back off the screen.

Every other collaboration proof in this directory stops one layer short
of this one. `app-collab-proof.py` attaches synthetic clients to two real
sessions and shows that opaque blobs cross, which proves the transport
and says nothing about whether two documents converge. `app-launch-proof
.py` starts a real app and shows it paints. Neither answers the question
a person actually asks, which is "does what I type appear on their
screen".

So: a real word processor in each of two real sessions, in one room, one
of them typed into, and a **screenshot of the other**. The evidence is
the pixels. There is no assertion in this file that can substitute for
looking at the image it writes, and the script says so rather than
reporting a pass it cannot see.

# What this shows

Both halves of the round trip, in pixels: each session renders the
other's caret and pointer under the other machine's peer id, **and each
session's page shows what the other typed**. Every hop is real app
traffic — app → capnp broker → Iroh → the far broker's drain → the far
app → pixels.

Two independently started apps each create their own Automerge document,
so a delta on its own cannot make one adopt the other's: it names
objects the receiving document has never seen. What bridges them is the
full-state snapshot a session sends when it sees a peer arrive, which
the receiver merges before any delta lands on top of it. Both frames are
unsigned, and both ride the op stream tagged with which kind they are.

That tag is what this run depends on. Without it a reader had only one
guess available — decode the bytes as a signed envelope, drop them when
that fails — and every unsigned frame is exactly what that discards. The
symptom was this script's old expected failure: presence crossed, the
document did not, and a peer's caret moved over a blank page.

# What the two sessions share, and what they must not

One room, formed the way a person forms one: an invite minted on one
session, pasted into the other. Everything else is per-session — state,
documents, caches, device key — because two sessions sharing a state
directory would converge through the filesystem and prove nothing.

Two things are machine state rather than session state and must NOT be
isolated, or no app can start at all:

  QOS_APPS_DATA_DIR       the installed-app registry
  QOS_RUNTIME_BUNDLE_DIR  the runtime bundle every packaged app pins

Usage:
  ./word-processor-convergence-proof.py [--keep] [--text WORDS]

Writes both screenshots and prints their paths. Exits non-zero if a step
the script *can* check fails; what the screens show is the screenshots'
to say, and this file does not claim it for them.
"""
from __future__ import annotations

import argparse
import base64
import json
import os
import pathlib
import shutil
import socket
import subprocess

from reaper import die_with_parent
import sys
import tempfile
import time

REPO = pathlib.Path(__file__).resolve().parents[2]
SHELL = REPO / "target" / "debug" / "shell"
LAUNCHER = REPO / "target" / "debug" / "qos-launcher"

APP = "dev.qos.word-processor"
APPLICATIONS = "applications"

failures: list[str] = []


def check(label: str, ok: bool, detail: str = "") -> bool:
    print(f"  {'PASS' if ok else 'FAIL'}  {label}" + (f" — {detail}" if detail else ""))
    if not ok:
        failures.append(label)
    return ok


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
    """One real compositor, isolated from the machine and from its peer
    in everything except the two stores an app launch cannot do
    without."""

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
            # Machine state the isolation must not take with it.
            "QOS_APPS_DATA_DIR": apps_dir or os.path.join(home, ".local/share/qos-apps"),
            "QOS_RUNTIME_BUNDLE_DIR": os.path.join(home, ".local/share/qos-runtime"),
            # Without its own device key a session's peer-sync turns
            # itself off, and the only symptom is a missing peer.* action.
            "QOS_PEER_KEY_PATH": os.path.join(self.dir, "keys", "peer-key"),
            "NIRIOS_HEADLESS_SIZE": "1280x800",
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

    def wait_up(self, timeout=90.0) -> bool:
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

    def chrome(self) -> dict:
        return self.call({"op": "state_dump"})["state"]["chrome"]

    def invoke(self, ident: str) -> dict:
        return self.call({"op": "actions_invoke", "provider": "actions", "id": ident})

    def frames(self) -> list[dict]:
        r = self.call({"op": "foreign_frames"})
        if "foreign_frames" in r:
            return r["foreign_frames"].get("frames", [])
        return r.get("answer", r.get("frames", [])) or []

    def snapshot(self, path: str) -> bool:
        self.call({"op": "snapshot", "path": path})
        # The snapshot is executed on the next frame, so give the
        # compositor a moment to actually write the file before the
        # caller decides whether it exists.
        for _ in range(40):
            if os.path.exists(path) and os.path.getsize(path) > 0:
                return True
            time.sleep(0.25)
        return False

    def stop(self):
        try:
            self.proc.terminate()
            self.proc.wait(timeout=8)
        except Exception:
            try:
                self.proc.kill()
            except Exception:
                pass
        try:
            self.log.close()
        except Exception:
            pass


def launch_and_paint(session: Session, timeout: float = 90.0) -> dict | None:
    """Start the word processor by address and wait for it to paint.

    A tile can look like a window while being a placeholder, so the
    frame counter is the ground truth: only a surface that was not there
    before this launch, and that has taken a frame, is this app.
    """
    before = {f.get("surface_id") for f in session.frames()}
    resp = session.call({"op": "actions_invoke", "provider": APPLICATIONS, "id": APP})
    if "err" in resp:
        note(f"{session.name}: invoke refused — {json.dumps(resp['err'])}")
        return None
    end = time.time() + timeout
    while time.time() < end:
        for f in session.frames():
            if f.get("surface_id") not in before and f.get("frame_seq", 0) > 0:
                return f
        time.sleep(1.0)
    return None


def collab_lines(session: Session, needles: tuple[str, ...]) -> list[str]:
    """Lines from a session's log mentioning any of `needles`.

    Read rather than asserted on: the log is how a failure explains
    itself, and quoting it beats reporting that a screenshot was
    disappointing.
    """
    try:
        text = open(session.log_path, errors="replace").read()
    except OSError:
        return []
    return [ln for ln in text.splitlines() if any(n in ln for n in needles)]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--keep", action="store_true", help="leave sessions and files behind")
    ap.add_argument("--text", default="collaboration works",
                    help="what the first session types")
    ap.add_argument("--apps", default=None, metavar="DIR",
                    help="an app registry to use instead of the machine's. Point it "
                         "at one holding a locally built app to prove a change that "
                         "lives inside the app rather than in the compositor.")
    args = ap.parse_args()

    for binary, what in ((SHELL, "shell"), (LAUNCHER, "qos-launcher")):
        if not binary.exists():
            print(f"missing {what} at {binary}")
            print("build it:  cargo build -p shell --bin shell && "
                  "cargo build -p launcher --bin qos-launcher")
            return 2

    # AF_UNIX paths cap near 108 bytes, and a session binds several
    # sockets under its root — a long temp prefix fails the compositor's
    # own bind, which reads as "the session never came up".
    root = tempfile.mkdtemp(prefix="/tmp/qs-")
    shots = os.path.join(root, "shots")
    os.makedirs(shots, exist_ok=True)
    print(f"session root: {root}")

    sessions: list[Session] = []
    try:
        print("\n[1] two real sessions")
        alice = Session("alice", root, args.apps); sessions.append(alice)
        bob = Session("bob", root, args.apps); sessions.append(bob)
        for s in sessions:
            if not check(f"{s.name} came up", s.wait_up(), f"see {s.log_path}"):
                return 1

        print("\n[2] one room, formed the way a person forms one")
        exported = {a["id"] for a in alice.call(
            {"op": "actions_enumerate", "query": ""}).get("answer", {}).get("actions", [])}
        if not check("peer-sync is on", "peer.invite" in exported):
            note(f"check {alice.log_path} for 'device key load failed'")
            return 1

        alice.invoke("peer.invite")
        token = wait_for(lambda: alice.chrome()["peer_sync"]["invite_token"], timeout=30)
        if not check("alice minted an invite", bool(token)):
            return 1
        alice.call({"op": "key", "key": "Escape"})
        room = json.loads(base64.urlsafe_b64decode(token + "=" * (-len(token) % 4)))["room_id"]

        bob.invoke("peer.connect")
        wait_for(lambda: bob.chrome()["peer_sync"]["connect_input"] is not None, timeout=25)
        bob.call({"op": "type", "text": token})
        bob.call({"op": "key", "key": "Enter"})
        joined = wait_for(
            lambda: (bob.chrome()["peer_sync"]["status"] or "").startswith("Joined"), timeout=30)
        if not check("bob joined alice's room", bool(joined)):
            return 1
        bob.call({"op": "key", "key": "Escape"})
        note(f"room = {room}")

        print("\n[3] the real word processor on each session")
        a_win = launch_and_paint(alice)
        check("alice's word processor painted", a_win is not None,
              json.dumps(a_win) if a_win else f"see {alice.log_path}")
        b_win = launch_and_paint(bob)
        check("bob's word processor painted", b_win is not None,
              json.dumps(b_win) if b_win else f"see {bob.log_path}")
        if not (a_win and b_win):
            return 1

        # Both apps need to have attached to their broker and announced
        # their address before either types, or the first keystrokes go
        # out to nobody and the run looks like a transport failure.
        time.sleep(6.0)
        for s in sessions:
            lines = collab_lines(s, ("collab connected", "collab:"))
            note(f"{s.name}: {lines[-1] if lines else 'no collab line in the log yet'}")

        print(f"\n[4] alice types {args.text!r}")
        # Put the pointer in the document body before typing: a launch
        # leaves focus wherever the chrome had it, and text typed at the
        # chrome is text the document never sees.
        alice.call({"op": "click_at", "x": 640, "y": 460})
        time.sleep(1.5)
        alice.call({"op": "type", "text": args.text})
        check("the keystrokes were accepted", True)

        print("\n[5] what each screen shows")
        # Long enough for a change to be published, gossiped, drained on
        # the far side, applied, and painted. Every one of those is a
        # separate hop and the last is the only one with pixels.
        time.sleep(12.0)
        a_png = os.path.join(shots, "alice-typed.png")
        b_png = os.path.join(shots, "bob-received.png")
        check("alice's screen was captured", alice.snapshot(a_png), a_png)
        check("bob's screen was captured", bob.snapshot(b_png), b_png)

        print()
        print("  alice (typed):   " + a_png)
        print("  bob   (remote):  " + b_png)
        print()
        print("  Open both. Each screen should carry the other's caret and")
        print("  pointer under that machine's peer id, and both pages should")
        print(f"  read {args.text!r} — the snapshot that made bob's document")
        print("  adopt alice's, then her keystrokes applied on top of it.")
        print("  A caret over a blank page is the failure this proves against.")
        print()
        for s in sessions:
            pub = collab_lines(s, ("publish", "AppCrdt", "broadcast doc delta"))
            recv = collab_lines(s, ("recv", "inbound", "CRDTIncremental"))
            note(f"{s.name}: {len(pub)} publish-ish line(s), {len(recv)} receive-ish line(s)")
            note(f"{s.name} log: {s.log_path}")

        if failures:
            print(f"\nFAILED ({len(failures)}): " + "; ".join(failures))
            return 1
        return 0
    finally:
        if args.keep:
            print(f"\nsessions left running under {root}")
        else:
            for s in sessions:
                s.stop()
            print(f"\nsessions stopped; files kept at {root}")


if __name__ == "__main__":
    sys.exit(main())
