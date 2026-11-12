#!/usr/bin/env python3
"""The session's real paths end to end, in one headless nested session, in under a minute.

One session (dist/dev/nested-session.sh, headless, lavapipe only) runs what a person's session
runs: the shell with its broker and workspace host, the launcher and its sandboxes, the
host, native Tasks, Chat and the agent runtime, and four tilolang apps built from apps/tl (Notes,
Tasks, Mail, Chat) and installed as signed packages. Every check below drives it the way a person,
the REPL or an agent does, and each one is there because a bug in that path was invisible to the
unit tests and found only by starting a session by hand:

  audio    the session's sound is its own: one null output, no device of the machine's in its
           graph, and the session manager answers (that output is the default sink).
           Catches WirePlumber's audio monitor enumerating the machine's ALSA cards inside the
           session: a card the host's PipeWire holds makes it retry without pause, spinning at
           nearly half a core for as long as the session runs, and Settings then lists hardware
           the session does not own.

  open     the four apps, opened at once onto the workspace's canvas, each present a first
           frame on the first try.
           Catches the broker, channel-key and workspace lock deadlock (fixed in b33cc45ace7):
           fails on 08636f201d9 (before the fix), where only one of the four apps presented a
           frame.
  canvas   every window the four apps opened stands on the canvas the arrangement publishes,
           and the window in front is drawn inside the screen's frame.
           Catches windows opened on the canvas being placed off screen (off the canvas, or
           outside the frame the person sees). Where the windows land depends on the order the
           four concurrent opens arrive in: on integration 70ba3301da0 this check failed in 3 of 6
           runs (windows at y = -484 and y = 788 on a 752 px high frame).
  repl     through the session's door (`tilolang repl`), on a session whose person named the
           program as they started it (`--admit-repl`, as naming it on the sheet would): the REPL
           reaches the four apps, reads the record the notes app's window shows, writes a note
           there by the names the app declares (`notes.new_note(notes.notebook)`), and changes
           the desk, whose published state says so.
           Catches the door handing a task an installed tilolang app's surface instead of its
           package: its actions were named by their stored ids and its root by nothing.
           Catches the door not forwarding record changes (fixed in 71382cec3d7): before the
           fix every change answered "this desktop applies no task's changes at a record's
           origin". No build has the deadlock fix without this one, so on 08636f201d9 the run
           stops at `open` first. Also catches a tilolang app's first change (its schema
           stamp) being committed without the Genesis claim, which the sealed channel refuses
           ("a document change carries no claim") and after which the workspace's readers miss the
           record: the check fails on any refused change, so it fails on integration
           70ba3301da0 until `open_stored` commits the Genesis (fix not landed yet).
  ask      asking an agent sends the person's request to the agent they keep — a new one when
           they keep none — through the session's one send (`session:send-to#send`, the action a
           window's box, text typed on the canvas and an agent all run). The session then starts
           that agent, it asks the model, and its answer lands in the person's conversation with
           it, where Chat shows it. The model is TestOnlyFixedReplyModel below: a loopback
           OpenAI-compatible endpoint inside this script that gives one fixed reply. It exists
           for this test only; never point a live session at it.
           Catches a send that never reaches the agent's conversation (the request then lies
           unanswered in Chat, as it did in a session with no Chat installed: "so.tilo.chat is
           not installed here"), an agent the session never starts, and a turn whose steps are
           refused: the model is never asked, and the check fails with what the session logged.
  restart  the session stopped and started again on the same state: the four windows come
           back, present frames, and hold what was written (the note in Notes, the channel in
           Chat).
  dev      the session started once more, and while its four windows come back four
           `tilolang dev` at once, each installing an app the session has not had (DEV_APPS):
           each window presents a first frame within DEV_BOUND (3 s) of its install; 0.5 to
           0.9 s here. Catches an app's first opening in a workspace waiting on the broker's
           invocation journal (its authorship grant was an invocation, answered once its
           admission and outcome were synced behind every other one, with a 30 s timeout as
           the way out), the dev tool waiting for the whole catalogue listing to settle, and
           concurrent `tilolang dev` writing over each other's builds: on 00c953abf01 no window
           presented a frame within 30 s.

Speed: one session serves every check; the restart and `dev` start it again. No check sleeps a
fixed time: each waits for the condition it needs (a listing answered with `--wait`, a frame
presented, a line from the REPL), bounded by HANG_BOUND seconds, and fails with what it saw.

Where it runs: a container on this machine, never on the remote builders. Tilo's sandboxes mount
a fresh /proc, which the builders' containers refuse (aae4effbff4), so no app can start there. The
session's build is the nested-session build of this tree (TILO_NESTED_APPS="agent tasks"): name
another with TILO_NESTED_SESSION_BUILD, or let --build make it (dist/dev/nested-session-build.sh,
through --remote-exec where this machine has no toolchain). Nothing runs it on a schedule: it takes
a full build and a session of its own, so a person starts it.

Usage:
  scripts/headless-examples/session-e2e.py [--build] [--out DIR] [--keep] [--only CHECK,...]

The verdict is OUT/summary.json (each check's result and seconds); the exit status is non-zero
when a check fails. Never touches the live desktop or this machine's GPU.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import select
import shlex
import shutil
import signal
import struct
import subprocess
import sys
import threading
import time
from pathlib import Path
from typing import Any, Callable

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
SESSION_SH = REPO / "dist/dev/nested-session.sh"
sys.path.insert(0, str(HERE / "agent-tilolang"))
from replay_model import OpenAiReplay  # noqa: E402

# The tilolang apps, by their directory under apps/tl, and the native apps beside them: the
# agent runtime that answers the person's asks, native Tasks, and Chat, which keeps the
# conversations an agent works in and the agents a workspace keeps.
TL_APPS = ["notes", "tasks", "mail", "chat"]
NATIVE = "agent tasks chat"
# The provider the session publishes its own actions under
# (tilo_actions::remote::SESSION_ACTIONS_PROVIDER).
SESSION_PROVIDER = "so.tilo.session"
# How long any one wait may take before it counts as a hang: a first frame takes well under a
# second here, so this bounds only a path that never answers.
HANG_BOUND = float(os.environ.get("TILO_TEST_HANG_BOUND_SECS", "30"))
# The one output a nested session's sound has (tilo_media::audio::SESSION_SINK).
SESSION_SINK = "tilo-sink"
# What the person asks the agent, and the one reply the test-only model gives. The request is a
# question, which an agent answers in its reply: a request for work in an application would have
# to be carried out there, and this model only answers.
REQUEST = "What is left for the release?"
FIXED_REPLY = "Three tasks are left in the release. (This is the e2e test model's fixed reply.)"
TEST_MODEL = "e2e-fixed-reply"
# Chat's messages as the workspace's record holds them: what was said and in which
# conversation (apps/chat/src/record.rs, `derived:messages`).
CHAT_MESSAGES = "derived:part.0@derived:messages@derived:default@tab:0000000000000000"
# The agent loop's own tool that finishes a turn with the answer its person reads
# (apps/agent/src/tools.rs, `PLAN`).
FINISH = "plan"
# What the session and its apps log when the ask or the run's own steps are refused
# (crates/session/session/src/send_to.rs, app_tabs.rs, apps.rs, apps/agent/src/session).
RUN_REFUSALS = [
    "an app action was not delivered",
    "a send did not land",
    "a window's send was not taken",
    "an agent did not start",
    "refused a run that answers no turn",
    "the conversation refused the run's report",
    "the conversation's `record-work` refused its arguments",
]


def app_id(name: str) -> str:
    return f"so.tilo.lang.{name}"


class Failed(Exception):
    pass


def run(argv: list, **kwargs) -> str:
    result = subprocess.run([str(a) for a in argv], text=True, capture_output=True, **kwargs)
    if result.returncode != 0:
        raise Failed(f"{' '.join(map(str, argv))} failed ({result.returncode}): "
                     f"{(result.stderr or result.stdout).strip()[-2000:]}")
    return result.stdout


def wait_for(what: str, check: Callable[[], Any], seen: Callable[[], Any] | None = None) -> Any:
    """`check()` once it answers truthy. Each try is a round trip to the session (a listing,
    a frame report), so the loop turns at the session's pace; it never sleeps a fixed time.
    Past HANG_BOUND it fails, with what `seen()` says the session showed."""
    end = time.monotonic() + HANG_BOUND
    while True:
        value = check()
        if value:
            return value
        if time.monotonic() > end:
            shown = f"; the session showed: {seen()}" if seen else ""
            raise Failed(f"no {what} within {HANG_BOUND:.0f} s{shown}"[:4000])
        # Yield between round trips; a try already waits on the session.
        time.sleep(0.02)


# --- the test-only model -------------------------------------------------------------------

class TestOnlyFixedReplyModel:
    """A loopback OpenAI-compatible endpoint for this test alone: every turn of the agent's loop
    finishes with FIXED_REPLY, and each turn it was asked is kept."""

    def __init__(self) -> None:
        self.task_turns: list[list[dict[str, Any]]] = []
        self.lock = threading.Lock()
        self.endpoint = OpenAiReplay(self)  # type: ignore[arg-type]
        self.errors = self.endpoint.errors

    # OpenAiReplay asks its replay's `respond`.
    @property
    def replay(self) -> "TestOnlyFixedReplyModel":
        return self

    def respond(self, messages: list[dict[str, Any]]) -> list[dict[str, Any]]:
        with self.lock:
            self.task_turns.append(messages)
            turn = len(self.task_turns)
        # The loop's finish tool: a turn that answers without an action is the person's reply. Its
        # call id is the turn's, as a provider's is, so a later turn's call is not taken for one
        # already consumed.
        return [{"type": "tool_use", "id": f"toolu_e2e_finish_{turn}", "name": FINISH,
                 "input": {"reply": FIXED_REPLY, "done": True}}]

    @property
    def port(self) -> int:
        return self.endpoint.port

    def record(self) -> str:
        """The provider record (profile, no key) that points a session at this endpoint."""
        return json.dumps({"profile": {
            "id": "e2e-test-only-model", "label": "E2E test-only model",
            "protocol": "open_ai_compatible", "endpoint": f"http://127.0.0.1:{self.port}/v1",
            "location": "this_device", "enabled": True,
            "models": [{"id": TEST_MODEL, "label": TEST_MODEL, "tool_calling": True}],
            "default_model": TEST_MODEL}, "key": None})

    def close(self) -> None:
        self.endpoint.close()


# --- the session ---------------------------------------------------------------------------

class Session:
    """The headless nested session on one state directory: started, stopped, started again."""

    def __init__(self, build: Path, state: Path, out: Path, tapps: list[Path]):
        self.build = build
        self.state = state
        self.out = out
        self.tapps = tapps
        self.proc: subprocess.Popen | None = None
        self.ready: dict = {}
        self.starts = 0

    def env(self) -> dict:
        s = self.state
        env = {k: v for k, v in os.environ.items() if k not in ("TILO_APPS_SYSTEM_DIR", "WAYLAND_DISPLAY")}
        env.update(
            HOME=str(s / "home"), XDG_CONFIG_HOME=str(s / "config"), XDG_DATA_HOME=str(s / "data"),
            XDG_STATE_HOME=str(s / "state"), XDG_CACHE_HOME=str(s / "cache"),
            TILO_IDENTITY_ROOT=str(s / "identity"), TILO_APPS_DATA_DIR=str(s / "apps"),
            TILO_RUNTIME_BUNDLE_DIR=str(s / "runtimes"), TILO_SECRETS_DIR=str(s / "secrets"),
            TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE=str(s / "keys/runtime-publishers.txt"),
        )
        if self.ready:
            env.update(TILO_TEST_CONTROL=self.ready["control"], XDG_RUNTIME_DIR=self.ready["runtime"])
        return env

    def bin(self, name: str) -> Path:
        # A read-only (shared) build runs from the session's own copy of its programs.
        own = self.state / "bin" / name
        return own if own.exists() else self.build / "bin" / name

    def script(self, extra: list) -> list:
        return [SESSION_SH, "--no-build", "--headless", "--software", "--apps", NATIVE,
                "--state", self.state, *extra]

    def start(self, install: bool) -> None:
        self.starts += 1
        tag = f"start-{self.starts}"
        ready = self.out / f"ready-{tag}.json"
        ready.unlink(missing_ok=True)
        argv = self.script(["--ready", ready])
        for tapp in self.tapps if install else []:
            argv += ["--install-tapp", tapp]
        # The person named the REPL's program as they started the session, as they would to use it
        # where no sheet is answered: what `tilolang repl` on this session asks for, their decision
        # made ahead of time (`--` ends the script's own options; the rest are the session's).
        argv += ["--", "--admit-repl", str(self.bin("tilolang"))]
        env = {**os.environ, "TILO_NESTED_SESSION_BUILD": str(self.build),
               "RUST_LOG": os.environ.get("RUST_LOG", "warn,tilo::launch_latency=info")}
        log = open(self.out / f"session-{tag}.log", "w")
        self.proc = subprocess.Popen([str(a) for a in argv], stdout=log, stderr=subprocess.STDOUT,
                                     start_new_session=True, env=env)
        # Starting includes installing the packages and minting the identity: a longer bound.
        end = time.monotonic() + 4 * HANG_BOUND
        while not ready.is_file():
            if self.proc.poll() is not None:
                raise Failed(f"the session ended ({self.proc.returncode}); see {log.name}")
            if time.monotonic() > end:
                raise Failed(f"the session did not answer; see {log.name}")
            time.sleep(0.05)
        self.ready = json.loads(ready.read_text())
        # A viewer is looking: headless frames are rasterized.
        self.control("raw", json.dumps({"op": "watch", "watching": True}))
        # No wait for the Home workspace to become native: an app opened before that must
        # still open on the first try, which is what `open` checks.

    def stop(self) -> None:
        proc, self.proc = self.proc, None
        if proc is not None and proc.poll() is None:
            try:
                os.killpg(proc.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                proc.wait(60)
            except subprocess.TimeoutExpired:
                os.killpg(proc.pid, signal.SIGKILL)
                proc.wait(10)
        log = self.state / "logs/session.log"
        if log.is_file():
            shutil.copy(log, self.out / f"shell-{self.starts}.log")
        self.ready = {}

    def log_lines(self) -> list[str]:
        try:
            return (self.state / "logs/session.log").read_text(errors="replace").splitlines()
        except OSError:
            return []

    def log_tail(self, lines: int = 20) -> str:
        try:
            text = (self.state / "logs/session.log").read_text(errors="replace").splitlines()
        except OSError:
            return "(no shell log)"
        return "\n".join(line[:300] for line in text[-lines:] if "WARN" in line or "ERROR" in line)

    # -- control --

    def control(self, *args: str) -> str:
        return run([self.bin("tilo-control"), *args], env=self.env())

    def control_json(self, *args: str) -> Any:
        return json.loads(self.control(*args))

    def state_dump(self) -> dict:
        return self.control_json("state-dump")

    def actions(self, query: str) -> list[dict]:
        """The actions listed for `query`, once the listing is complete (no app index or
        directory page still on its way)."""
        return self.control_json("actions", "list", query, "--json", "--programmatic",
                                 "--wait").get("actions", [])

    def invoke(self, provider: str, ident: str, args: dict | None = None,
               job: bool = False) -> dict:
        """Invoke `provider`'s action `ident` and answer its settled receipt. `job` for an action
        that takes the work and carries it out on the session's next pass: its receipt says
        `accepted`, and the check proves what it did by the session's own state."""
        argv = ["actions", "invoke", provider, ident, "--wait=30000"]
        if args is not None:
            argv += ["--args-json", json.dumps(args)]
        answer = self.control_json(*argv)
        state = answer.get("receipt", {}).get("update", {}).get("state")
        done = (None, "succeeded", "accepted") if job else (None, "succeeded")
        if state not in done:
            raise Failed(f"{provider} {ident} did not succeed: {json.dumps(answer)[:1500]}")
        return answer

    def windows(self) -> list[dict]:
        return self.control_json("windows", "--json")

    def frames(self) -> list[dict]:
        """The open windows that show one, each with where it stands (`rect`) and how many frames
        its client drew (`frames`): a window showing the same thing keeps its count, so a rise
        between two reads is a frame it drew."""
        return [w for w in self.windows() if w.get("window") is not None]

    def provision(self, model: TestOnlyFixedReplyModel) -> None:
        """Give the session's person the test-only model as their provider, as Settings' "Add
        provider" writes it. While the session runs: it applies the write as its own."""
        run([self.bin("tilo-model-gateway"), "provision-provider", "--record"],
            input=model.record(), env=self.env())


# --- what the checks share ---------------------------------------------------------------

def presented(session: Session) -> set[str]:
    """The apps whose window is drawn: mapped on an output, with a frame its client drew."""
    return {f["app_id"] for f in session.frames() if f.get("rect") and f.get("frames", 0) > 0}


def windows_by_app(session: Session) -> dict[str, dict]:
    return {w.get("app_id"): w for w in session.windows()}


def said(session: Session, wait_ms: int = 0) -> list[dict]:
    """Chat's messages as the workspace's record holds them, each by the words it says and the
    conversation it is in: what a person reads in Chat, read through the record door as they
    read it. Held until the record moves or `wait_ms` passed when one is given."""
    answer = session.control_json("raw", json.dumps({
        "op": "record_read", "app": "so.tilo.chat", "records": [CHAT_MESSAGES],
        "wait_ms": wait_ms}))
    messages: list[dict] = []

    def walk(value: Any) -> None:
        if isinstance(value, dict):
            if isinstance(value.get("text"), str) and isinstance(value.get("channel"), str):
                messages.append({"text": value["text"], "channel": value["channel"]})
            for inner in value.values():
                walk(inner)
        elif isinstance(value, list):
            for inner in value:
                walk(inner)

    walk(answer.get("answer") or {})
    return messages


class Repl:
    """`tilolang repl` on the session's door, one entry at a time. Off a terminal the REPL
    echoes each line it reads after its prompt (`tl> entry`), so an entry's answer is what it
    prints between that echo and the echo of the empty line sent after it. Its banner comes after
    the line saying what the door waits on (`asking to act as you: ...`)."""

    PROMPT = "tl> "

    def __init__(self, session: Session, out: Path):
        ready = out / "repl-ready.json"
        ready.write_text(json.dumps(session.ready))
        self.proc = subprocess.Popen(
            [str(session.bin("tilolang")), "repl", "--session", str(ready), "--color", "never"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
            env={**os.environ, "XDG_STATE_HOME": str(out / "repl-state")})
        self.transcript = ""
        self.pending = ""
        # The banner: the door it is on and the apps it reaches.
        self.banner = self._read_until(lambda text: text if "apps: " in text else None)

    def _read_until(self, done: Callable[[str], Any]) -> Any:
        """Read the REPL's output until `done(text read so far)` answers; what it answered."""
        assert self.proc.stdout is not None
        fd = self.proc.stdout.fileno()
        end = time.monotonic() + HANG_BOUND
        while not (found := done(self.pending)):
            left = end - time.monotonic()
            if left <= 0:
                raise Failed(f"the REPL answered nothing within {HANG_BOUND:.0f} s: {self.pending[-2000:]!r}")
            ready, _, _ = select.select([fd], [], [], left)
            if ready:
                chunk = os.read(fd, 65536)
                if not chunk:
                    raise Failed(f"the REPL ended: {(self.transcript + self.pending)[-3000:]}")
                self.pending += chunk.decode(errors="replace")
        return found

    def __call__(self, entry: str) -> str:
        assert self.proc.stdin is not None
        self.proc.stdin.write(f"{entry}\n\n".encode())
        self.proc.stdin.flush()
        echo, blank = f"{self.PROMPT}{entry}\n", f"{self.PROMPT}\n"

        def answered(text: str) -> str | None:
            at = text.find(echo)
            if at < 0:
                return None
            stop = text.find(blank, at + len(echo))
            return None if stop < 0 else text[at + len(echo):stop]

        answer = self._read_until(answered)
        cut = self.pending.find(blank, self.pending.find(echo) + len(echo)) + len(blank)
        self.transcript += self.pending[:cut]
        self.pending = self.pending[cut:]
        if answer.startswith("error:"):
            raise Failed(f"the REPL refused `{entry}`: {answer}")
        return answer

    def close(self) -> None:
        self.transcript += self.pending
        if self.proc.poll() is None:
            assert self.proc.stdin is not None
            self.proc.stdin.close()
            try:
                self.proc.wait(10)
            except subprocess.TimeoutExpired:
                self.proc.kill()


# --- the checks ----------------------------------------------------------------------------

def check_audio(session: Session) -> dict:
    """The session's sound is its own: one null output, none of the machine's devices."""
    env = session.env()
    props = [entry.get("info", {}).get("props", {}) for entry in json.loads(run(["pw-dump"], env=env))]
    hardware = sorted({str(p.get("device.name") or p.get("node.name")) for p in props
                       if p.get("device.api") in ("alsa", "bluez5")})
    if hardware:
        raise Failed(f"the session's sound enumerates the machine's hardware {hardware}: its "
                     "WirePlumber must leave the audio and Bluetooth monitors off")
    sinks = sorted(str(p.get("node.name")) for p in props if p.get("media.class") == "Audio/Sink")
    if sinks != [SESSION_SINK]:
        raise Failed(f"the session's outputs are {sinks}, not only its own {SESSION_SINK}")
    default = run(["wpctl", "inspect", "@DEFAULT_AUDIO_SINK@"], env=env)
    if SESSION_SINK not in default:
        raise Failed(f"the session's output is not the default sink; wpctl says {default[-800:]}")
    return {"sinks": sinks}


def check_open(session: Session) -> dict:
    """The four apps opened at once, onto the desktop, each with a first frame, first try."""
    answers: dict[str, Any] = {}

    def open_app(name: str) -> None:
        try:
            answers[name] = session.invoke("applications", app_id(name))
        except Failed as error:
            answers[name] = error

    began = time.monotonic()
    threads = [threading.Thread(target=open_app, args=(n,)) for n in TL_APPS]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()
    refused = {n: str(a) for n, a in answers.items() if isinstance(a, Failed)}
    if refused:
        raise Failed(f"opening refused: {refused}")
    wanted = {app_id(n) for n in TL_APPS}
    wait_for("first frame of every app", lambda: wanted <= presented(session),
             lambda: f"presented {sorted(presented(session))}; windows {sorted(windows_by_app(session))}\n"
                     f"{session.log_tail()}")
    return {"seconds_to_every_first_frame": round(time.monotonic() - began, 2),
            "windows": sorted(wanted)}


def canvas_places(session: Session) -> dict[str, dict]:
    """Where each window group of the desk stands on its workspace's canvas, by group: the
    arrangement's own rows, as the desk publishes them (`arrangement.places` of each workspace,
    a place being where its top-leading corner is and how wide and tall it stands there)."""
    tree = session.state_dump().get("workspace_tree") or {}
    places: dict[str, dict] = {}
    for workspace in tree.get("workspaces") or []:
        for group, place in (workspace.get("arrangement") or {}).get("places", {}).items():
            places[str(group)] = place
    return places


def screen_frame(path: Path) -> tuple[int, int]:
    """The size of the screen a snapshot of the session shows: a PNG states it in its header."""
    header = path.read_bytes()[:24]
    if header[:8] != b"\x89PNG\r\n\x1a\n":
        raise Failed(f"{path} is not a PNG of the screen")
    return struct.unpack(">II", header[16:24])


def check_canvas(session: Session) -> dict:
    """Every window the four apps opened stands on the canvas the arrangement publishes, and the
    window in front — the one the desk shows — is drawn inside the screen's frame.

    Catches an opening placed off the canvas (where the person would go looking for it and find
    nothing) and a desk that draws the window it points at off screen. Where windows land
    depends on the order the four concurrent opens arrive in: on integration 70ba3301da0 the
    shell drew them at y = -484 and y = 788 on a 752 px high frame."""
    shot = session.out / "canvas.png"
    session.control("snapshot", str(shot))
    width, height = screen_frame(shot)
    places = canvas_places(session)
    windows = windows_by_app(session)
    missing = []
    on_canvas = {}
    for name in TL_APPS:
        row = windows.get(app_id(name)) or {}
        place = places.get(str(row.get("group")))
        if place is None:
            missing.append({"app": app_id(name), "place": None, "row": row})
            continue
        if place["x"] < 0 or place["y"] < 0 or place["width"] <= 0 or place["height"] <= 0:
            missing.append({"app": app_id(name), "place": place})
            continue
        on_canvas[app_id(name)] = place
    if missing:
        raise Failed(f"not placed on the canvas: {missing}")
    # The window in front: the tab the active strip and group hold, which the desk draws.
    front = [w for w in session.windows() if w.get("active") and w.get("window") is not None]
    if not front:
        raise Failed(f"no window is in front: {sorted(windows)}")
    off = []
    for window in front:
        rect = window.get("rect")
        if not rect or not window.get("frames"):
            off.append({"app": window.get("app_id"), "drawn": None, "rect": rect})
            continue
        x, y, w, h = rect
        if x < 0 or y < 0 or x + w > width + 0.5 or y + h > height + 0.5:
            off.append({"app": window.get("app_id"), "screen": rect})
    if off:
        raise Failed(f"the window in front is not drawn inside the {width}x{height} screen: "
                     f"{off}; places {places}")
    return {"frame": [0, 0, width, height], "placed": len(on_canvas),
            "front": [w.get("app_id") for w in front]}


def check_repl(session: Session, out: Path) -> dict:
    """The door's REPL reaches the four apps, reads one's record, and changes the desk the
    session publishes, which the desk draws.

    The record it reads is the notes app's notebook, and the actions it lists are the app's
    own. It writes a note there by the names the app declares (`notes.new_note(notes.notebook)`,
    as the app's own source says it), and reads it back. It changes the desk as it may:
    `session.overview` flips a surface of the desk, read back from the state dump the desk
    publishes."""
    repl = Repl(session, out)
    try:
        # The REPL says what the door waits on before it is in: its admission is the person's,
        # made on the sheet or ahead of time with `--admit-repl`, never answered by the REPL.
        if "asking to act as you" not in repl.banner:
            raise Failed(f"the REPL gave itself the door without asking: {repl.banner.strip()!r}")
        apps = re.search(r"apps: ([^(]*)", repl.banner)
        if not apps or not all(n in apps.group(1) for n in TL_APPS):
            raise Failed(f"the REPL does not reach the four apps: {repl.banner.strip()}")
        notebook = repl("ls notes")
        if "notes.Notebook" not in notebook or "#read" not in notebook:
            raise Failed(f"the notes app's record is not reachable: {notebook!r}")
        # A note, written as the person by the names the notes app declares, and counted back
        # in the notebook it went into.
        def notes_held() -> int:
            counted = repl("notes.notebook.notes.len()")
            found = re.search(r"^\s*(\d+)\s*$", counted, re.M)
            if not found:
                raise Failed(f"the notebook's notes are not counted: {counted!r}")
            return int(found.group(1))

        before = notes_held()
        written = repl("notes.new_note(notes.notebook)")
        after = notes_held()
        if after != before + 1:
            raise Failed(f"notes.new_note(notes.notebook) wrote no note ({before} then {after}): "
                         f"{written!r}")
        desk = repl("ls session")
        if "session" not in desk:
            raise Failed(f"the session's own actions are not listed: {desk[:600]!r}")
        # A change of the desk, as the person typing it: the overview's surface goes down and
        # up again, and the desk's own published state says so each time.
        def overview() -> bool:
            return bool(session.state_dump()["chrome"]["overlays"]["overview"])

        for wanted in (False, True):
            answer = repl(f"session.overview(open: .some({str(wanted).lower()}))")
            if "ok" not in answer:
                raise Failed(f"session.overview(open: .some({str(wanted).lower()})) failed: {answer!r}")
            wait_for(f"the overview {'up' if wanted else 'down'} in the desk's state",
                     lambda: overview() == wanted)
        # The workspace admitted every change the apps and the REPL made: a change the sealed
        # channel refuses (the first, unnamed change of a tilolang app's record) is dropped
        # with only this line to say so, and its record is missing wherever it is read.
        refused = [line[:400] for line in session.log_lines() if "change carries no claim" in line]
        if refused:
            raise Failed(f"the workspace refused {len(refused)} change(s) as carrying no claim: {refused[0]}")
        return {"asked": True, "notebook": notebook.strip().splitlines()[0],
                "notes": [before, after], "overview": "down and up again"}
    finally:
        repl.close()
        (out / "repl-transcript.txt").write_text(repl.transcript)


def refusals(session: Session) -> list[str]:
    """What the session and its apps logged as refusing the ask or a step of the run it made."""
    return [line for line in session.log_lines() if any(r in line for r in RUN_REFUSALS)]


def check_ask(session: Session, model: TestOnlyFixedReplyModel) -> dict:
    """Asking an agent sends the person's request to the agent they keep (a new one when they
    keep none) through the session's one send, and the agent the session starts answers it: it
    asks the model, and its answer lands in the person's conversation with it, where Chat shows
    it."""
    session.provision(model)
    # The action every ask runs: the one a window's box and text typed on the canvas invoke, and
    # the one the desk's catalogue lists for an agent to run.
    row = wait_for("the session's send, as the desk lists it", lambda: next((
        a for a in session.actions("send")
        if a.get("provider") == SESSION_PROVIDER and a.get("operation") == "send"), None),
        lambda: [{"provider": a.get("provider"), "title": a.get("title"),
                  "operation": a.get("operation")} for a in session.actions("send")])
    session.invoke(row["provider"], row["id"],
                   {"to": [{"kind": "new-agent", "name": ""}], "text": REQUEST}, job=True)

    def reached_the_agent() -> bool:
        """Whether the agent was asked, failing at once with what the session refused."""
        if refused := refusals(session):
            raise Failed("the ask or the run's own steps were refused:\n" + "\n".join(refused[:8]))
        return bool(model.task_turns)

    wait_for("the agent the ask made, asking the model", reached_the_agent,
             lambda: f"model errors {model.errors}\n{session.log_tail()}")

    def answered() -> dict | None:
        """The agent's answer, in the conversation the person's request is in: where a person
        reads it."""
        messages = said(session)
        asked = next((m for m in messages if m["text"] == REQUEST), None)
        if asked is None:
            return None
        return next((m for m in messages
                     if m["channel"] == asked["channel"] and m["text"].strip() == FIXED_REPLY), None)

    answer = wait_for("the agent's answer in the person's conversation with it", answered,
                      lambda: f"turns {len(model.task_turns)}; the conversation holds "
                              f"{[m['text'][:80] for m in said(session)]}\n{session.log_tail()}")
    if refused := refusals(session):
        raise Failed("the ask or the run's own steps were refused:\n" + "\n".join(refused[:8]))
    return {"send": row["id"], "turns": len(model.task_turns),
            "answer": answer["text"][:120]}


def check_restart(session: Session) -> dict:
    """Started again on the same state: the four windows come back, each with a frame and a place
    on the canvas the arrangement publishes."""
    session.stop()
    began = time.monotonic()
    session.start(install=False)
    wanted = {app_id(n) for n in TL_APPS}
    wait_for("every window back with a frame", lambda: wanted <= presented(session),
             lambda: f"presented {sorted(presented(session))}; windows {sorted(windows_by_app(session))}\n"
                     f"{session.log_tail()}")
    places = canvas_places(session)
    unplaced = sorted(a for a in wanted
                      if str((windows_by_app(session).get(a) or {}).get("group")) not in places)
    if unplaced:
        raise Failed(f"back without a place on the canvas: {unplaced}; places {places}")
    return {"seconds_to_windows": round(time.monotonic() - began, 2)}


# The tilolang apps `tilolang dev` installs into the running session, ones it has not had yet,
# each by a `tilolang dev` of its own, all at once.
DEV_APPS = ["counter", "today", "board", "views"]
# How long after its install each app's window may take to present its first frame.
DEV_BOUND = float(os.environ.get("TILO_TEST_DEV_OPEN_BOUND_SECS", "3.0"))


class DevRun:
    """One `tilolang dev` of `name`, its output read on a thread of its own: when it said the
    app was installed and when it said it was open."""

    def __init__(self, session: Session, name: str, ready: Path) -> None:
        env = {**session.env(), "TILO_NESTED_SESSION_BUILD": str(session.build),
               "XDG_STATE_HOME": str(session.out / "tilolang-state")}
        self.name = name
        self.installed: float | None = None
        self.opened: float | None = None
        self.lines: list[str] = []
        self.log = open(session.out / f"dev-{name}.log", "w")
        self.proc = subprocess.Popen(
            [str(session.bin("tilolang")), "dev", str(REPO / "apps/tl" / name),
             "--search", str(REPO / "apps/tl"), "--session", str(ready), "--repo", str(REPO)],
            cwd=REPO, env=env, stdout=self.log, stderr=subprocess.PIPE, text=True)
        self.reader = threading.Thread(target=self.read, daemon=True)
        self.reader.start()

    def read(self) -> None:
        assert self.proc.stderr is not None
        for line in self.proc.stderr:
            now = time.monotonic()
            self.lines.append(line)
            self.log.write(line)
            self.log.flush()
            # The packages an app uses install before it; its own install is the last before
            # it opens.
            if "installed in" in line and self.opened is None:
                self.installed = now
            if "is open" in line and self.opened is None:
                self.opened = now

    def stop(self) -> None:
        self.proc.terminate()
        self.proc.wait(10)
        self.reader.join(10)
        self.log.close()


def check_dev(session: Session) -> dict:
    """The session started again, and while its four windows come back (a desktop just signed
    in to), one `tilolang dev` for each of DEV_APPS at once, apps the session has not had: each
    window presents its first frame within DEV_BOUND of its install."""
    session.stop()
    session.start(install=False)
    ready = session.out / f"ready-start-{session.starts}.json"
    runs = [DevRun(session, name, ready) for name in DEV_APPS]
    framed: dict[str, float] = {}
    try:
        def every_window() -> bool:
            now = time.monotonic()
            shown = presented(session)
            for run in runs:
                if run.name not in framed and app_id(run.name) in shown:
                    framed[run.name] = now
                if run.proc.poll() is not None and run.name not in framed:
                    raise Failed(f"`tilolang dev` for {run.name} ended: {''.join(run.lines)[-2000:]}")
            return len(framed) == len(runs)

        wait_for("every dev app's first frame", every_window,
                 lambda: f"framed {sorted(framed)}\n{session.log_tail()}")
    finally:
        for run in runs:
            run.stop()
    result: dict[str, Any] = {}
    late = []
    for run in runs:
        if run.installed is None:
            raise Failed(f"`tilolang dev` showed {run.name} without installing it: {''.join(run.lines)}")
        seconds = round(framed[run.name] - run.installed, 3)
        result[run.name] = {"seconds_install_to_first_frame": seconds,
                            "seconds_install_to_open": None if run.opened is None
                            else round(run.opened - run.installed, 3)}
        if seconds > DEV_BOUND:
            late.append(f"{run.name} {seconds} s")
    if late:
        raise Failed(f"first frames later than {DEV_BOUND} s after the install: {', '.join(late)}; "
                     f"{json.dumps(result)}\n{session.log_tail()}")
    return result


# --- the run -------------------------------------------------------------------------------

CHECKS = ["audio", "open", "canvas", "repl", "ask", "restart", "dev"]


def build_session(out: Path, remote: str | None = None) -> Path:
    """The nested-session build of this tree, compiled on the arm builder.

    Through `remote` (a command prefix, e.g. a remote-exec wrapper), the build runs where
    this machine cannot: a program that forwards the builder's environment, not this one's,
    carries the app list in its own."""
    target = out / "nested-session"
    command = ["bash", str(REPO / "dist/dev/nested-session-build.sh"), str(target)]
    argv = [*shlex.split(remote), "env", f"TILO_NESTED_APPS={NATIVE}", *command] if remote else command
    subprocess.run(argv, cwd=REPO, check=True, env={**os.environ, "TILO_NESTED_APPS": NATIVE})
    return target


def prepare(build: Path, state: Path, out: Path) -> list[Path]:
    """A state directory with nothing in it but the runtime bundle and keys of earlier runs
    (baked again only when the build changed), and the four apps built against that runtime."""
    if state.exists():
        for child in state.iterdir():
            if child.name not in ("runtimes", "keys"):
                shutil.rmtree(child) if child.is_dir() and not child.is_symlink() else child.unlink()
    env = {**os.environ, "TILO_NESTED_SESSION_BUILD": str(build)}
    run([SESSION_SH, "--no-build", "--prepare-only", "--headless", "--software", "--apps", NATIVE,
         "--state", state], env=env)
    runtime = (state / "runtimes/.version").read_text().strip()
    built = out / "build"
    tapps = []
    # A read-only (shared) build runs from the session's own copy of its programs; a writable
    # one from where it was built.
    tilolang = next(p for p in (state / "bin/tilolang", build / "bin/tilolang") if p.exists())
    for name in TL_APPS:
        run([tilolang, "build", REPO / "apps/tl" / name, "--search", REPO / "apps/tl",
             "--out", built, "--tilo-runtime", f"tilo-runtime/{runtime}"])
        tapps.append(built / f"{name}.tapp")
    return tapps


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--build", action="store_true", help="build the nested session of this tree first")
    parser.add_argument("--remote-exec", default=os.environ.get("TILO_NESTED_SESSION_BUILD_EXEC"),
                        help="command prefix the session's build runs through (default "
                             "$TILO_NESTED_SESSION_BUILD_EXEC)")
    parser.add_argument("--out", type=Path, default=REPO / "target/session-e2e")
    parser.add_argument("--keep", action="store_true", help="leave the session running at the end")
    parser.add_argument("--only", help="comma list of checks to run (default: all, in order)")
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=True)
    for stale in out.glob("*.log"):
        stale.unlink()
    began = time.monotonic()
    build = build_session(out, args.remote_exec) if args.build else Path(
        os.environ.get("TILO_NESTED_SESSION_BUILD", REPO / "target/nested-session")).resolve()
    if not (build / "bin/tilo-session").is_file():
        print(f"session-e2e: no nested-session build at {build} (pass --build, or set "
              "TILO_NESTED_SESSION_BUILD)", file=sys.stderr)
        return 2
    only = args.only.split(",") if args.only else CHECKS
    # Every check but dev looks at the four apps' windows and instances, which open starts.
    if any(name in only for name in ("canvas", "repl", "ask", "restart")) and "open" not in only:
        only = ["open", *only]
    summary: dict[str, Any] = {"build": str(build), "checks": {}, "failed": []}
    model = TestOnlyFixedReplyModel()
    session: Session | None = None
    try:
        state = out / "state"
        tapps = prepare(build, state, out)
        summary["prepare_seconds"] = round(time.monotonic() - began, 1)
        session = Session(build, state, out, tapps)
        session.start(install=True)
        summary["start_seconds"] = round(time.monotonic() - began, 1)
        steps: dict[str, Callable[[], dict]] = {
            "audio": lambda: check_audio(session),
            "open": lambda: check_open(session),
            "canvas": lambda: check_canvas(session),
            "repl": lambda: check_repl(session, out),
            "ask": lambda: check_ask(session, model),
            "restart": lambda: check_restart(session),
            "dev": lambda: check_dev(session),
        }
        for name in CHECKS:
            if name not in only:
                continue
            print(f"==> {name}", file=sys.stderr, flush=True)
            step_began = time.monotonic()
            try:
                result = steps[name]()
                result["seconds"] = round(time.monotonic() - step_began, 2)
                summary["checks"][name] = result
                print(f"ok   {name} ({result['seconds']} s)", file=sys.stderr, flush=True)
            except Failed as error:
                summary["failed"].append(name)
                summary["checks"][name] = {"failed": str(error),
                                           "seconds": round(time.monotonic() - step_began, 2)}
                print(f"FAIL {name}: {error}", file=sys.stderr, flush=True)
                # Nothing after a failed open has windows to look at.
                if name == "open":
                    break
    except (Failed, subprocess.CalledProcessError) as error:
        summary["failed"].append("session")
        summary["error"] = str(error)
        print(f"FAIL session: {error}", file=sys.stderr)
    finally:
        summary["seconds"] = round(time.monotonic() - began, 1)
        (out / "summary.json").write_text(json.dumps(summary, indent=2))
        if session is not None and not args.keep:
            session.stop()
        model.close()
    verdict = "FAILED: " + ", ".join(summary["failed"]) if summary["failed"] else "every check passed"
    print(f"==> {verdict} in {summary['seconds']} s (summary: {out / 'summary.json'})", file=sys.stderr)
    return 1 if summary["failed"] else 0


if __name__ == "__main__":
    sys.exit(main())
