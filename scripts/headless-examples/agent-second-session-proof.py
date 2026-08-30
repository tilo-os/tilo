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

# What counts as proof here

Both sessions open the **same file off disk**, so the two pages agreeing
*before* the agent runs proves nothing whatsoever about replication.
They would agree if the room did not exist. The only moment that tests
the room is the agent's edit, which exists in one app's document and has
to cross to the other.

So the payoff is read out of Xaver's own app rather than left to the
frames. A late joiner announces the same `(app, room, record)` the word
processor announced and sends `Presence { Arrived }`; every toolkit
answers that with its whole document. What comes back is produced *by
the app running in that session, out of the document it is holding* —
not the wire being read back to itself.

Two things are asserted about it and neither is worth anything alone:

  * the draft's title is still there — a blob that does not carry it is
    not the document, and "the instruction is gone" from a document that
    was never the draft is a claim about nothing;
  * the line addressed to Claude is gone, and the headings the agent
    made are there.

That second half is what tests the room, and the disk is the control:
the agent removes the instruction in a replica living in a third
process and never saves, so the file the two apps opened still carries
the line — which this script checks. An app that no longer holds it did
not read that from the disk.

The frames are still worth opening. They are no longer the only
evidence.

# If the edit never arrives

Check whether the machine was busy before suspecting the room. These
are heavyweight debug binaries and the compositor renders in software
here; on a box already running a large build this fails at step 3 with
"the word processor never painted" and the app's own log is *empty*,
because it never got far enough to write to it. That is starvation, not
a defect — `uptime` first. The same load makes the agent sit forever
waiting for a document that is still loading, which is why it now
bounds that wait and names the address instead (`--await-document-ms`).

An app started through the catalogue's install-and-open path has also
been seen to come up without connecting to collaboration while presence
still crosses — so the room looks alive and nothing is shared. If a run
fails with both apps painted and presence crossing, check each session's
log for `collab connected` before suspecting the agent.

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
import threading
import time

from reaper import die_with_parent, track

REPO = pathlib.Path(__file__).resolve().parents[2]


PROFILES = ("demo", "debug")


def _tool(name: str) -> pathlib.Path | None:
    """The newest build of `name` across the profiles this repo uses.

    For the standalone tools only. They speak JSON over a Unix socket
    and share no linkage with anything, so the newest is simply the
    most current answer.
    """
    found = [p for p in (REPO / "target" / prof / name
                         for prof in PROFILES) if p.exists()]
    return max(found, key=lambda p: p.stat().st_mtime) if found else None


def _matched_pair() -> tuple[pathlib.Path | None, pathlib.Path | None]:
    """The compositor and the app, from **one** profile.

    These two are not interchangeable across profiles: they share the
    toolkit and the Wayland/collaboration surface between them, and a
    demo-profile app against a debug-profile shell died on its first
    frame with `Io error: Broken pipe` — which reads as "the app never
    painted", i.e. as a fault in the app or the room rather than in the
    pairing. Choose a profile that has both, newest first, and say
    which one was chosen so a run is never ambiguous about what it
    exercised.
    """
    best: tuple[float, pathlib.Path, pathlib.Path] | None = None
    for prof in PROFILES:
        shell = REPO / "target" / prof / "shell"
        app = REPO / "target" / prof / "word-processor"
        if not (shell.exists() and app.exists()):
            continue
        age = min(shell.stat().st_mtime, app.stat().st_mtime)
        if best is None or age > best[0]:
            best = (age, shell, app)
    return (best[1], best[2]) if best else (None, None)


def _pair_from(profile: str) -> tuple[pathlib.Path | None, pathlib.Path | None]:
    """The compositor and the app from one named profile, or `(None, None)`."""
    shell = REPO / "target" / profile / "shell"
    app = REPO / "target" / profile / "word-processor"
    return (shell, app) if shell.exists() and app.exists() else (None, None)


SHELL, WORD_PROCESSOR = _matched_pair()
AGENT = _tool("qos-agent")
# Decodes an Automerge blob to the strings it holds. Without it there is
# no honest way to answer "does this document say that" — see `_strings`.
DOC_TEXT = _tool("qos-doc-text")

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

# The first line, used as a positive control: a blob that does not carry
# it is not the draft, and "the instruction is absent" from a document
# that was never the draft is worth nothing.
TITLE = "Quarterly review"
# The instruction the agent consumes. Matched on its tail rather than the
# whole line because the agent's name is a prefix a caret pill also
# carries, and the claim is about the *instruction* being carried out.
INSTRUCTION_TAIL = "make this a proper document"

# What `launch_word_processor` returns when the app is up and
# collaborating but has not produced a frame. Distinct from `None`
# (which means it is not running at all) because the document claims
# still hold and only the screenshots are lost.
UNPAINTED = "unpainted"

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
        """The typed collaboration broker this session binds."""
        return os.path.join(self.runtime, "qos", "services", "collab.sock")

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
    Wayland display and an inherited typed-service fd — the same contract as
    the real launcher. Both sessions open the *same* record path so the apps
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
        "QOS_PACKAGE_ID": APP,
        "QOS_WORKSPACE_ID": room,
        "QUICK_APP_ID": session.name,
        "QOS_USER_ID": session.name,
        "RUST_LOG": "warn",
        # Makes the toolkit say whether it reached the broker. Without it
        # an app that is up but not collaborating looks exactly like an
        # app that is up and collaborating, and the difference is the
        # whole question this script asks.
        "QUICK_COLLAB_DEBUG": "1",
    })
    log_path = os.path.join(session.dir, "word-processor.log")
    before = {f.get("surface_id") for f in session.frames()}
    service = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    service.connect(session.broker)
    env["QOS_SVC_COLLAB_FD"] = str(service.fileno())
    proc = track(subprocess.Popen(
        [str(WORD_PROCESSOR), brief], env=env,
        stdout=subprocess.DEVNULL, stderr=open(log_path, "w"),
        pass_fds=(service.fileno(),),
        preexec_fn=die_with_parent(),
    ))
    service.close()
    end = time.time() + timeout
    while time.time() < end:
        if proc.poll() is not None:
            note(f"{session.name}: the word processor exited; see {log_path}")
            return None
        for f in session.frames():
            if f.get("surface_id") not in before and f.get("frame_seq", 0) > 0:
                return f
        time.sleep(1.0)

    # No frame. That is fatal to the screenshots and not necessarily
    # fatal to the document: these are heavyweight debug binaries and the
    # compositor renders in software, so on a loaded machine the app
    # spends minutes short of its first frame while its collaboration is
    # already up. Those are two different claims and they should not
    # share one gate — a run that can still answer "what does xaver's app
    # hold" is worth more than a run that stops because it could not take
    # a picture of it.
    try:
        connected = "collab connected=true" in open(log_path, errors="replace").read()
    except OSError:
        connected = False
    if connected and proc.poll() is None:
        note(f"{session.name}: the word processor is connected but has not "
             f"painted; continuing for the document, not for the frames")
        return UNPAINTED
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


class DocumentReader:
    """Ask one session's word processor what its document actually says.

    The compositor cannot answer this. `state_dump` knows a foreign
    surface exists and painted; it has no idea what is written on it,
    and the shell's search providers only read the shell's *own* text
    apps. So this asks the app, in the app's own protocol: it speaks the
    length-prefixed JSON `CollabRequest`/`CollabEvent` the toolkit
    speaks, announces the same `(app_id, cluster_id, record)` address
    the word processor announced, and sends the one frame every toolkit
    answers with its whole document — `Presence { Arrived }`, the same
    handshake `qos-agent` opens with.

    What comes back is therefore produced *by the app running in that
    session, out of the document that app is holding*. It is not the
    wire being read back to itself.

    The address has to match exactly. The broker's `delivers_to` makes
    the record the innermost test and it fails closed both ways, so a
    reader that announces no record is not a reader of an app that
    announced one — it is a reader of a different, empty conversation,
    and it reports a blank page as confidently as a real one.
    """

    # The presence scope the toolkit answers on. Same constant the agent
    # uses; a different scope is a different conversation and gets no reply.
    SCOPE = "word-processor"

    def __init__(self, sock_path: str, room: str, record: str | None,
                 handle: str = "readback") -> None:
        self.frames: list[dict] = []
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(10.0)
        self.sock.connect(sock_path)
        self._send({"Hello": {"app_id": APP, "cluster_id": room,
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
        except Exception:                   # the session went away
            pass

    def close(self) -> None:
        try:
            self.sock.close()
        except OSError:
            pass

    def documents(self) -> list[bytes]:
        """Every whole-document frame somebody in the room answered with."""
        return [bytes(m["CRDTUpdate"]["data"])
                for m in list(self.frames) if "CRDTUpdate" in m]

    def await_settled(self, must_have: str, must_lack: str, timeout: float):
        """Wait for a document that says `must_have` and not `must_lack`.

        Both halves are needed and neither is sufficient. `must_lack`
        alone is what an empty page, a wrong address and a dropped
        connection all look like, so on its own it proves nothing at
        all; `must_have` is the positive control that says the blob
        being searched really is the draft. Only a document with one and
        not the other is the agent's edit having arrived.

        More than one participant answers `Arrived` with a full
        document on this address, so every frame is searched rather
        than the newest — reading only the last one reports somebody
        else's copy as the app's.

        Returns `(ok, strings_of_the_best_frame)`.
        """
        deadline = time.time() + timeout
        last: list[str] = []
        while time.time() < deadline:
            for blob in self.documents():
                lines = _strings(blob)
                body = "\n".join(lines)
                if must_have in body:
                    last = lines
                    if must_lack not in body:
                        return True, lines
            time.sleep(0.5)
        return False, last


def _strings(blob: bytes) -> list[str]:
    """Every string an Automerge blob holds, as `<path>\\t<text>` lines.

    Not a byte search. Automerge DEFLATE-compresses each column of a
    change chunk once it is over a few hundred bytes, so a phrase a
    person typed is genuinely in the document and genuinely absent from
    its bytes — a harness that grepped the bytes reported an empty page
    for a document that had the sentence in it, and failed that way
    whatever had gone wrong.

    Block kinds are stored as plain string values (`BLOCK_KIND`), so
    this shows structure as well as prose: after the agent's pass the
    dump carries `heading` where it carried `paragraph`.
    """
    if DOC_TEXT is None:
        return []
    try:
        done = subprocess.run([str(DOC_TEXT), "-"], input=blob,
                              stdout=subprocess.PIPE,
                              stderr=subprocess.DEVNULL, timeout=20)
    except (OSError, subprocess.SubprocessError):
        return []
    if done.returncode != 0:
        return []
    return done.stdout.decode(errors="replace").splitlines()


def headings(lines: list[str]) -> int:
    """How many blocks in this dump are headings rather than paragraphs."""
    return sum(1 for ln in lines if ln.rsplit("\t", 1)[-1].strip() == "heading")


def main() -> int:
    ap = argparse.ArgumentParser(description="An agent's edit, on somebody else's screen.")
    ap.add_argument("--keep", action="store_true", help="leave the sessions running")
    ap.add_argument("--no-model", action="store_true",
                    help="decide by keyword instead of asking a model")
    ap.add_argument("--shots", default=None, metavar="DIR",
                    help="where the two screenshots go (default: under the session root)")
    ap.add_argument("--profile", default="auto", choices=("auto", *PROFILES),
                    help="which build of the compositor and the app to run "
                         "(they must match; default: the newest profile that "
                         "has both). Pin one when another build is rewriting "
                         "the other profile underneath the run.")
    args = ap.parse_args()

    global SHELL, WORD_PROCESSOR
    if args.profile != "auto":
        SHELL, WORD_PROCESSOR = _pair_from(args.profile)
        if SHELL is None:
            print(f"target/{args.profile} has no shell + word-processor pair")
            return 2

    for binary, what in ((SHELL, "shell"), (AGENT, "qos-agent"),
                         (WORD_PROCESSOR, "word-processor")):
        if binary is None:
            print(f"missing {what} in target/demo and target/debug")
            print("build it:  cargo build -p shell -p agent -p word-processor")
            return 2
    if DOC_TEXT is None:
        print("missing qos-doc-text in target/demo and target/debug")
        print("build it:  cargo build -p qdoc --bin qos-doc-text")
        return 2
    for binary, what in ((SHELL, "shell"), (AGENT, "qos-agent"),
                         (WORD_PROCESSOR, "word-processor")):
        print(f"  {what}: {binary}")

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
        painted = True
        for s in sessions:
            win = launch_word_processor(s, room, brief)
            if not check(f"{s.name}'s word processor is up", win is not None,
                         f"see {s.log_path}"):
                return 1
            if win is UNPAINTED:
                painted = False
        check("both apps produced a frame, so the screenshots mean something",
              painted,
              "no frame under load — the document claims below still stand, "
              "the pictures do not")

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

        # The baseline, read out of xaver's own app. Everything claimed
        # after the agent runs is a change from *this*, measured the same
        # way, so "it was already like that" is not available as an
        # explanation of the result.
        record = f"file:{brief}"
        base = DocumentReader(xaver.broker, room, record, handle="baseline")
        try:
            deadline = time.time() + 30
            before_lines: list[str] = []
            while time.time() < deadline:
                for blob in base.documents():
                    lines = _strings(blob)
                    if any(INSTRUCTION_TAIL in ln for ln in lines):
                        before_lines = lines
                        break
                if before_lines:
                    break
                time.sleep(0.5)
        finally:
            base.close()
        if not check("xaver's app answered with the draft", bool(before_lines),
                     "no document frame carried the draft"):
            return 1
        before_headings = headings(before_lines)
        check("and it is a flat draft — no headings in it yet",
              before_headings == 0, f"{before_headings} heading block(s)")

        print("\n[5] the agent joins quinten's session and chooses")
        argv = [str(AGENT), "--endpoint", quinten.broker, "--workspace", room,
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
        print("\n[7] what xaver's own app says it is holding")
        # The payoff, and the only step that tests the room at all.
        #
        # Both apps opened the same file, so their agreeing *beforehand*
        # proves nothing — they would agree with no room at all. What
        # cannot be explained that way is the instruction line going
        # away: the agent removed it in a replica that lives in a third
        # process, never saved, and the file on disk still has the line
        # in it. If xaver's app no longer holds it, it did not read that
        # from the disk. It came through the room.
        reader = DocumentReader(xaver.broker, room, record, handle="readback")
        try:
            ok, after_lines = reader.await_settled(
                must_have=TITLE, must_lack=INSTRUCTION_TAIL, timeout=60)
        finally:
            reader.close()

        check("xaver's document still holds the draft it started from",
              any(TITLE in ln for ln in after_lines), TITLE)
        check("the line addressed to the agent is gone from xaver's copy", ok,
              "still present" if after_lines else "no document frame arrived")
        after_headings = headings(after_lines)
        check("and his copy carries the headings the agent made",
              after_headings > before_headings,
              f"{before_headings} before, {after_headings} after")
        # The file the two apps opened is untouched, which is what makes
        # the two claims above about the room rather than about a disk.
        on_disk = open(brief, errors="replace").read()
        check("the file on disk still carries the instruction, unsaved",
              INSTRUCTION_TAIL in on_disk,
              "the app saved, so disk is no longer a control")

        print(f"\n  xaver before: {before_png}")
        print(f"  xaver after:  {x_png}")
        print(f"  quinten:      {q_png}")
        if painted:
            print("\nThe frames are evidence too, and worth opening: the claims")
            print("above are about what the apps hold, and a person still has to")
            print("look at whether xaver's page renders it.")
        else:
            print("\nThe frames are NOT evidence in this run: neither app reached")
            print("its first paint, so the screenshots show a room without pages")
            print("in it. What the apps hold is still asserted above; what they")
            print("draw was not established. Re-run on an idle machine for that.")

        if failures:
            print(f"\nFAILED ({len(failures)}): " + "; ".join(failures))
            return 1
        print("\nPASS — the agent's edit is in xaver's copy, and xaver's session")
        print("never ran the agent, typed the change, or re-read the file.")
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
