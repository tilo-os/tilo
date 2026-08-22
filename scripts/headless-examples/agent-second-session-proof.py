#!/usr/bin/env python3
"""An edit the agent made, on somebody else's screen.

`agent-demo.py` proves the agent's work reaches the word processor that
holds the document — a separate process with its own replica, which is
real, but it is still one machine. The beat the demo rests on is the
other one: Quinten writes a line asking for something, and **Xaver's**
screen changes. The agent has no screen of its own, so the agent's copy
was never the shot.

So: two real compositors with separate identities, homes and device
keys, joined into one room the way a person joins one — an invite minted
on the first and pasted into the second. A real word processor in each.
Quinten types a flat draft and one line addressed to Claude. Then
`qos-agent` joins Quinten's session, chooses an action with a model from
the catalogue the word processor exports, and carries it out — and
Xaver's session is asked what *his* copy says.

# STATUS: this does not pass yet, and the reason is not the agent

As of writing, the agent half works and the second session does not.
Quinten's page gets its title and headings with Claude's caret pill on
it; Xaver's page is unchanged, instruction line and all, while Xaver's
*presence* shows up on Quinten's screen and Quinten's on Xaver's. A room
that looks alive while nothing in it is shared is a known shape of
defect here; do not read a failure of this script as a defect in the
agent until the two apps are shown to share a document at all.

Read that last part carefully, because this script's setup hides it:
both sessions open the **same file off disk**, so the two pages agreeing
*before* the agent runs proves nothing whatsoever about replication.
They would agree if the room did not exist. The only moment that tests
the room is the agent's edit, which exists in one app's document and has
to cross to the other.

# What counts as proof here, and what does not

Xaver's screen is the evidence, and this file does not claim what it
shows. It captures his screen twice — before the agent runs and after —
and says to go and look at them.

There is deliberately no text assertion about Xaver's copy, because
there is no honest one available. The obvious one does not work: the
shell's search providers read the bodies of the shell's *own* text apps
out of its tab documents, and a packaged app keeps its document in the
app process, so they cannot answer for the word processor at all.
Comparing his two frames byte for byte does not work either — a blinking
caret changes them. So this script checks what it genuinely can (both
apps painted, the agent chose from the catalogue and exited clean) and
refuses to call the run a pass on the strength of it.

# If the edit never arrives

Launch both apps from the palette (this script does). An app started
through the catalogue's install-and-open path is known to come up
without connecting to collaboration, while presence still crosses — so
the room looks alive and nothing is shared. If a run fails with both
apps painted and presence crossing, check each session's log for
`collab connected` before suspecting the agent.

Usage:
  ./agent-second-session-proof.py [--keep] [--no-model]

Writes both screenshots and prints their paths. Exits non-zero if a
claim it can check fails.
"""
from __future__ import annotations

import argparse
import base64
import json
import os
import pathlib
import re
import socket
import subprocess
import sys
import tempfile
import time

from reaper import die_with_parent, track

REPO = pathlib.Path(__file__).resolve().parents[2]
SHELL = REPO / "target" / "debug" / "shell"
AGENT = REPO / "target" / "debug" / "qos-agent"
WORD_PROCESSOR = REPO / "target" / "debug" / "word-processor"

APP = "dev.qos.word-processor"
APPLICATIONS = "applications"

# A flat draft: no markup at all, a title line, two section labels, prose
# under each. Nothing here names an action — which one answers the last
# line is the model's to choose.
DRAFT = """Quarterly review

Revenue

We closed the quarter ahead of plan, with most of the gain arriving late.

Risks

Two suppliers remain single-sourced and that has not moved since March.

Claude: make this a proper document
"""

failures: list[str] = []


def check(label: str, ok: bool, detail: str = "") -> bool:
    print(f"  {'PASS' if ok else 'FAIL'}  {label}" + (f" — {detail}" if detail else ""))
    if not ok:
        failures.append(label)
    return bool(ok)


def note(msg: str) -> None:
    print(f"  ....  {msg}")


def wait_for(pred, timeout=30.0, poll=0.4):
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

    def __init__(self, name: str, root: str):
        self.name = name
        self.dir = os.path.join(root, name)
        os.makedirs(self.dir, exist_ok=True)
        self.socket = os.path.join(self.dir, "control.sock")
        self.runtime = os.path.join(self.dir, "run")
        os.makedirs(self.runtime, mode=0o700, exist_ok=True)
        home = os.path.expanduser("~")
        env = {
            **os.environ,
            "QOS_TEST_CONTROL": self.socket,
            "QOS_STATE_PATH": os.path.join(self.dir, "config", "state.json"),
            "XDG_DATA_HOME": os.path.join(self.dir, "data"),
            "XDG_CACHE_HOME": os.path.join(self.dir, "cache"),
            "XDG_STATE_HOME": os.path.join(self.dir, "state"),
            "XDG_CONFIG_HOME": os.path.join(self.dir, "config"),
            "XDG_RUNTIME_DIR": self.runtime,
            # Machine state the isolation must not take with it.
            "QOS_APPS_DATA_DIR": os.path.join(home, ".local/share/qos-apps"),
            "QOS_RUNTIME_BUNDLE_DIR": os.path.join(home, ".local/share/qos-runtime"),
            # Without its own device key a session's peer-sync turns
            # itself off, and the only symptom is a missing peer.* action.
            "QOS_PEER_KEY_PATH": os.path.join(self.dir, "keys", "peer-key"),
            "NIRIOS_HEADLESS_SIZE": "1600x1200",
            # The sandbox does not clear the environment, so a de_DE host
            # renders the app's chrome in German. Any frame this writes
            # could end up as footage, and mixed languages read as broken.
            "LC_ALL": "en_US.UTF-8",
            "LANG": "en_US.UTF-8",
            "LANGUAGE": "en_US",
        }
        env.pop("DISPLAY", None)
        env.pop("WAYLAND_DISPLAY", None)
        self.env = env
        self.log_path = os.path.join(self.dir, "shell.log")
        self.log = open(self.log_path, "w")
        self.proc = track(subprocess.Popen(
            [str(SHELL), "--backend", "headless", "--enable-peer-sync"],
            env=env, stdout=self.log, stderr=subprocess.STDOUT,
            preexec_fn=die_with_parent(),
        ))

    @property
    def broker(self) -> str:
        """The JSON collaboration broker this session binds."""
        return os.path.join(self.runtime, "qos", "services", "collab-json.sock")

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

    def invoke(self, provider: str, ident: str) -> dict:
        return self.call({"op": "actions_invoke", "provider": provider, "id": ident})

    def enumerate(self, query: str = "") -> list:
        r = self.call({"op": "actions_enumerate", "query": query})
        return r.get("answer", {}).get("actions", []) or []

    def frames(self) -> list[dict]:
        r = self.call({"op": "foreign_frames"})
        if "foreign_frames" in r:
            return r["foreign_frames"].get("frames", [])
        return r.get("answer", r.get("frames", [])) or []

    def snapshot(self, path: str) -> bool:
        self.call({"op": "snapshot", "path": path})
        for _ in range(40):
            if os.path.exists(path) and os.path.getsize(path) > 0:
                return True
            time.sleep(0.25)
        return False

    def log_text(self) -> str:
        try:
            return open(self.log_path, errors="replace").read()
        except OSError:
            return ""

    def stop(self):
        for fn in (self.proc.terminate, self.proc.kill):
            try:
                fn()
                self.proc.wait(timeout=8)
                break
            except Exception:
                continue
        try:
            self.log.close()
        except Exception:
            pass


def launch_word_processor(session: Session, room: str, brief: str,
                          timeout: float = 120.0):
    """Start the real word processor on `brief` as a client of `session`.

    Launched directly rather than through the palette, with the session's
    Wayland display and JSON broker in its environment — the launch the
    single-session agent harness uses, whose collaboration is known to
    work. Both sessions open the *same* record path so the two apps
    announce one address and the room can bridge them.
    """
    display_file = os.path.join(session.runtime, "qos-session", "wayland-display")
    if not wait_for(lambda: os.path.exists(display_file), timeout=45):
        note(f"{session.name}: no wayland display file at {display_file}")
        return None
    display = open(display_file).read().strip()
    if not wait_for(lambda: os.path.exists(session.broker), timeout=45):
        note(f"{session.name}: no collaboration broker at {session.broker}")
        return None

    session.call({"op": "place_next_window", "app_id": APP})
    env = dict(session.env)
    env.update({
        "WAYLAND_DISPLAY": display,
        "QUICK_COLLAB_SOCK": session.broker,
        "QOS_PACKAGE_ID": APP,
        "QOS_WORKSPACE_ID": room,
        "QUICK_APP_ID": session.name,
        "QOS_USER_ID": session.name,
        "RUST_LOG": "warn",
    })
    log_path = os.path.join(session.dir, "word-processor.log")
    before = {f.get("surface_id") for f in session.frames()}
    proc = track(subprocess.Popen(
        [str(WORD_PROCESSOR), brief], env=env,
        stdout=subprocess.DEVNULL, stderr=open(log_path, "w"),
        preexec_fn=die_with_parent(),
    ))
    end = time.time() + timeout
    while time.time() < end:
        if proc.poll() is not None:
            note(f"{session.name}: the word processor exited; see {log_path}")
            return None
        for f in session.frames():
            if f.get("surface_id") not in before and f.get("frame_seq", 0) > 0:
                return f
        time.sleep(1.0)
    note(f"{session.name}: the word processor never painted; see {log_path}")
    return None


def body_carries(sess: Session, token: str) -> bool:
    """Does this session's own document contain `token`?

    Asked through the search providers, which match a query against each
    tab's document body — the session answering about its own state.
    """
    for entry in sess.enumerate(token):
        if entry.get("provider") in ("notes", "editor", "text", "text_app"):
            return True
    return False


def main() -> int:
    ap = argparse.ArgumentParser(description="An agent's edit, on somebody else's screen.")
    ap.add_argument("--keep", action="store_true", help="leave the sessions running")
    ap.add_argument("--no-model", action="store_true",
                    help="decide by keyword instead of asking a model")
    ap.add_argument("--shots", default=None, metavar="DIR",
                    help="where the two screenshots go (default: under the session root)")
    args = ap.parse_args()

    for binary, what in ((SHELL, "shell"), (AGENT, "qos-agent")):
        if not binary.exists():
            print(f"missing {what} at {binary}")
            print("build it:  cargo build -p shell -p agent -p word-processor")
            return 2

    # AF_UNIX paths cap near 108 bytes and a session binds several sockets
    # under its root, so a long prefix fails the compositor's own bind and
    # reads as "the session never came up".
    root = tempfile.mkdtemp(prefix="/tmp/qs-")
    shots = args.shots or os.path.join(root, "shots")
    os.makedirs(shots, exist_ok=True)
    print(f"session root: {root}")

    sessions: list[Session] = []
    agent = None
    try:
        print("\n[1] two real sessions")
        quinten = Session("quinten", root); sessions.append(quinten)
        xaver = Session("xaver", root); sessions.append(xaver)
        for s in sessions:
            if not check(f"{s.name} came up", s.wait_up(), f"see {s.log_path}"):
                return 1

        print("\n[2] one room, formed the way a person forms one")
        exported = {a["id"] for a in quinten.enumerate("")}
        if not check("peer-sync is on", "peer.invite" in exported):
            note(f"check {quinten.log_path} for 'device key load failed'")
            return 1
        quinten.invoke("actions", "peer.invite")
        token = wait_for(lambda: quinten.chrome()["peer_sync"]["invite_token"], timeout=30)
        if not check("quinten minted an invite", bool(token)):
            return 1
        quinten.call({"op": "key", "key": "Escape"})
        room = json.loads(base64.urlsafe_b64decode(token + "=" * (-len(token) % 4)))["room_id"]

        xaver.invoke("actions", "peer.connect")
        wait_for(lambda: xaver.chrome()["peer_sync"]["connect_input"] is not None, timeout=25)
        xaver.call({"op": "type", "text": token})
        xaver.call({"op": "key", "key": "Enter"})
        joined = wait_for(
            lambda: (xaver.chrome()["peer_sync"]["status"] or "").startswith("Joined"), timeout=30)
        if not check("xaver joined quinten's room", bool(joined)):
            return 1
        xaver.call({"op": "key", "key": "Escape"})
        note(f"room = {room}")

        print("\n[3] the same document open in each session")
        brief = os.path.join(root, "brief.md")
        with open(brief, "w") as fh:
            fh.write(DRAFT)
        for s in sessions:
            win = launch_word_processor(s, room, brief)
            if not check(f"{s.name}'s word processor painted", win is not None,
                         f"see {s.log_path}"):
                return 1

        # Both apps must have attached to their broker and announced
        # themselves before anyone types, or the first keystrokes go out
        # to nobody and the run looks like a transport failure.
        time.sleep(8.0)
        for s in sessions:
            hits = [ln for ln in s.log_text().splitlines() if "collab connected" in ln]
            note(f"{s.name}: {hits[-1].strip() if hits else 'no collab line in the log'}")

        print("\n[4] the draft, before anyone has asked for anything")
        # Long enough for the draft to be published, gossiped, drained on
        # the far side, applied and painted. Every hop is separate and
        # only the last one has pixels.
        time.sleep(14.0)
        before_png = os.path.join(shots, "xaver-before.png")
        check("xaver's screen before the agent was captured", xaver.snapshot(before_png),
              before_png)

        print("\n[5] the agent joins quinten's session and chooses")
        argv = [str(AGENT), "--socket", quinten.broker, "--workspace", room,
                "--record", f"file:{brief}",
                "--name", "Claude", "--once", "--pace-ms", "600"]
        if args.no_model:
            argv.append("--no-model")
        agent_log = os.path.join(root, "agent.log")
        agent = track(subprocess.Popen(
            argv, env={**quinten.env, "RUST_LOG": "info"},
            stdout=subprocess.DEVNULL, stderr=open(agent_log, "w"),
            preexec_fn=die_with_parent(),
        ))
        try:
            code = agent.wait(timeout=120)
        except subprocess.TimeoutExpired:
            check("the agent finished", False, f"see {agent_log}")
            return 1
        if not check("the agent exited cleanly", code == 0, f"exit {code}; see {agent_log}"):
            print(open(agent_log, errors="replace").read()[-2000:])
            return 1

        log = re.sub(r"\x1b\[[0-9;]*m", "", open(agent_log, errors="replace").read())
        decided = re.search(r'decided_by="?(\w+)"?.*?\blive=(\w+)', log)
        if not check("the agent reported how it chose", bool(decided), f"see {agent_log}"):
            return 1
        route, live = decided.group(1), decided.group(2) == "true"
        want = "keywords" if args.no_model else "model"
        check(f"the {want} path chose the action", route == want,
              f"reported {route!r} (live={live})")
        why = re.search(r"claude-[\w.-]+: ([^\n]*?) block=", log)
        if why:
            note(f"it chose because: {why.group(1)}")

        print("\n[6] what the two screens show now")
        # The agent's last edit has to cross the same hops the draft did.
        time.sleep(14.0)
        q_png = os.path.join(shots, "quinten-after.png")
        x_png = os.path.join(shots, "xaver-after.png")
        check("quinten's screen was captured", quinten.snapshot(q_png), q_png)
        check("xaver's screen after the agent was captured", xaver.snapshot(x_png), x_png)
        print(f"\n  xaver before: {before_png}")
        print(f"  xaver after:  {x_png}")
        print(f"  quinten:      {q_png}")
        print("\nThe frames are the evidence and this script does not claim it for")
        print("them: open the two xaver frames and see whether his page changed.")

        if failures:
            print(f"\nFAILED ({len(failures)}): " + "; ".join(failures))
            return 1

        # Everything this script can check has passed, and the thing it
        # exists to show is not among them. Saying PASS here would be
        # claiming Xaver's screen changed, which only the frames can say
        # — and on the runs so far it has not.
        print("\nINCONCLUSIVE — the checkable steps all passed.")
        print("Whether the agent's edit reached xaver is in the frames, not here.")
        print("If his page is unchanged, look at whether the two apps ever shared")
        print("a document: they open the same file, so agreeing beforehand is not")
        print("evidence of a room, and the agent is the last thing to suspect.")
        return 0
    finally:
        if agent is not None and agent.poll() is None:
            agent.kill()
        if not args.keep:
            for s in sessions:
                s.stop()
        else:
            print(f"\n--keep: sessions left running under {root}")


if __name__ == "__main__":
    sys.exit(main())
