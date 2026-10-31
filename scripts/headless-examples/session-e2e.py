#!/usr/bin/env python3
"""The session's real paths end to end, in one headless nested session, in under a minute.

One session (dist/dev/nested-session.sh, headless, lavapipe only) runs what a person's session
runs: the shell with its broker and room host, the launcher and its sandboxes, the tilolang host,
native Tasks and the agent runtime, and four tilolang apps built from apps/tl (Notes, Tasks,
Mail, Chat) and installed as signed packages. Every check below drives it the way a person, the
REPL or an agent does, and each one is there because a bug in that path was invisible to the
unit tests and found only by starting a session by hand:

  open     the four apps, opened at once onto the workspace's canvas, each present a first
           frame on the first try.
           Catches the broker, channel-key and room lock deadlock (fixed in b33cc45ace7):
           fails on 08636f201d9 (before the fix), where only one of the four apps presented a
           frame.
  canvas   every window placed on the canvas is drawn on screen, inside the canvas's frame.
           Catches windows opened on the canvas being placed off screen (no fix landed yet).
           Where the windows land depends on the order the four concurrent opens arrive in: on
           integration 70ba3301da0 this check failed in 3 of 6 runs (windows at y = -484 and
           y = 788 on a 752 px high frame).
  repl     through the session's door (`tilolang repl`): a note and a chat channel written,
           `find` and `grep` return them, `tasks.add` makes a task the running Tasks instance
           lists, and `undo` takes it back out of that instance; the open Tasks window draws
           each, with nothing else waking it (a new frame of its window after each change).
           Catches the door not forwarding record changes (fixed in 71382cec3d7): before the
           fix every change answered "this desktop applies no task's changes at a record's
           origin". No build has the deadlock fix without this one, so on 08636f201d9 the run
           stops at `open` first. Also catches a tilolang app's first change (its schema
           stamp) being committed without the Genesis claim, which the sealed channel refuses
           ("a document change carries no claim") and after which the room's readers miss the
           record: the check fails on any refused change, so it fails on integration
           70ba3301da0 until `open_stored` commits the Genesis (fix not landed yet).
  ask      asking an agent makes its issue in the tracker, opens no window, and starts a task
           agent, which asks the model. The model is TestOnlyFixedReplyModel below: a loopback
           OpenAI-compatible endpoint inside this script that gives one fixed reply. It exists
           for this test only; never point a live session at it.
           Catches the ask addressing native Tasks' `tasks.tracker` while the windowless Tasks
           publishes under its bound alias (`tilo-ui-bound:v1:...`), so the broker never found
           it and the composer said "No reply yet": fails until that fix lands.
  restart  the session stopped and started again on the same state: the four windows come
           back, present frames, and hold what was written (the note in Notes, the channel in
           Chat).
  dev      the session started once more, and while its four windows come back four
           `tilolang dev` at once, each installing an app the session has not had (DEV_APPS):
           each window presents a first frame within DEV_BOUND (3 s) of its install; 0.5 to
           0.9 s here. Catches an app's first opening in a room waiting on the broker's
           invocation journal (its authorship grant was an invocation, answered once its
           admission and outcome were synced behind every other one, with a 30 s timeout as
           the way out), the dev tool waiting for the whole catalogue listing to settle, and
           concurrent `tilolang dev` writing over each other's builds: on 00c953abf01 no window
           presented a frame within 30 s.

Speed: one session serves every check; the restart and `dev` start it again. No check sleeps a
fixed time: each waits for the condition it needs (a listing answered with `--wait`, a frame
presented, a line from the REPL), bounded by HANG_BOUND seconds, and fails with what it saw.

Where it runs: on this machine, never on the remote builders. Tilo's sandboxes mount a fresh
/proc, which the builders' containers refuse (aae4effbff4), so no app can start there. The
session's build is the nested-session build of this tree: TILO_NESTED_SESSION_BUILD, else
target/nested-session; --build makes it first (dist/dev/nested-session-build.sh, compiled on
the arm builder). tools/ci/test-changed.sh runs it for changes to the shell, collaboration, the
tilolang host or tilolang-tilo.

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
import shutil
import signal
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
from replay_model import TASK_MARK, OpenAiReplay, text_of  # noqa: E402

# The tilolang apps, by their directory under apps/tl, and the native apps beside them: the
# agent runtime and native Tasks, whose tracker holds the agents' tasks.
TL_APPS = ["notes", "tasks", "mail", "chat"]
NATIVE = "agent tasks"
TRACKER_ISSUES = "tasks.tracker/issues/"
# tilo_actions::BOUND_RESOURCE_PREFIX: an app instance's alias of its own resource ids.
BOUND_RESOURCE_PREFIX = "tilo-ui-bound:v1:"
# How long any one wait may take before it counts as a hang: a first frame takes well under a
# second here, so this bounds only a path that never answers.
HANG_BOUND = float(os.environ.get("TILO_TEST_HANG_BOUND_SECS", "30"))
# What the person asks the agent, and the one reply the test-only model gives.
REQUEST = "Tidy up the release tasks"
FIXED_REPLY = "The release tasks are tidy. (This is the e2e test model's fixed reply.)"
TEST_MODEL = "e2e-fixed-reply"
# The task the test model's program adds: the change the asked agent makes in an app.
ASKED_TASK = "Call Xaver about the launch video"
# What the session logs as the task agent's process exits: a task agent ends with its run.
AGENT_EXITED = "app-exit: package=so.tilo.agent "
# What the agent logs when the issue refuses one of the run's own calls on it
# (apps/agent/src/session/tracker.rs, session.rs, agent_loop.rs).
RUN_REFUSALS = [
    "the issue did not take the run's report",
    "the issue's action was not invoked",
    "the issue refused the turn's receipt",
    "the issue refused the failed turn's receipt",
    "native task stopped before completion",
    "native task review report was refused",
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
    """A loopback OpenAI-compatible endpoint for this test alone: the composer's `respond`
    transform is told the request is work (titled as asked), and every turn of the task's own
    loop gets FIXED_REPLY and no tool call. It records each task turn it was asked."""

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
        if not any(TASK_MARK in text_of(m.get("content")) for m in messages):
            return [{"type": "text", "text": json.dumps({"result": {"work": REQUEST}})}]
        with self.lock:
            self.task_turns.append(messages)
            first = not any(m.get("role") == "assistant" for m in messages)
        # Its first turn adds a task through the `tilo` tool (apps/agent/src/programs.rs), the
        # way a real model does what it is asked; once that answered, the fixed reply ends it.
        if first:
            return [{"type": "tool_use", "id": "toolu_e2e_add_task", "name": "tilo",
                     "input": {"program": f'tasks.add(tasks.tasks, "{ASKED_TASK}")'}}]
        return [{"type": "text", "text": FIXED_REPLY}]

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
        log = self.state / "logs/shell.log"
        if log.is_file():
            shutil.copy(log, self.out / f"shell-{self.starts}.log")
        self.ready = {}

    def log_lines(self) -> list[str]:
        try:
            return (self.state / "logs/shell.log").read_text(errors="replace").splitlines()
        except OSError:
            return []

    def log_tail(self, lines: int = 20) -> str:
        try:
            text = (self.state / "logs/shell.log").read_text(errors="replace").splitlines()
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

    def invoke(self, provider: str, ident: str, args: dict | None = None) -> dict:
        argv = ["actions", "invoke", provider, ident, "--wait=30000"]
        if args is not None:
            argv += ["--args-json", json.dumps(args)]
        answer = self.control_json(*argv)
        state = answer.get("receipt", {}).get("update", {}).get("state")
        if state not in (None, "succeeded"):
            raise Failed(f"{provider} {ident} did not succeed: {json.dumps(answer)[:1500]}")
        return answer

    def windows(self) -> list[dict]:
        return self.control_json("windows", "--json")

    def frames(self) -> list[dict]:
        return self.control_json("raw", '{"op":"foreign_frames"}')["foreign_frames"]["frames"]

    def provision(self, model: TestOnlyFixedReplyModel) -> None:
        """Give the session's person the test-only model as their provider, as Settings' "Add
        provider" writes it. While the session runs: it applies the write as its own."""
        run([self.bin("tilo-model-gateway"), "provision-provider", "--record"],
            input=model.record(), env=self.env())


# --- what the checks share ---------------------------------------------------------------

def presented(session: Session) -> set[str]:
    return {f["app_id"] for f in session.frames() if f.get("presented") and f.get("pixels_ready")}


def windows_by_app(session: Session) -> dict[str, dict]:
    return {w.get("app_id"): w for w in session.windows()}


def read_content(session: Session, name: str) -> str | None:
    """What the running instance of tilolang `name` holds, as its "Read the content" answers:
    the room's records and, read one by one, each record of the app's own kinds. The instance
    in the workspace in front answers it, from its own replica."""
    action = wait_for(f"{name}'s 'Read the content'", lambda: next((
        a for a in session.actions("Read the content")
        if a.get("application") == app_id(name)
        and a["id"].split("\x1f")[-2:] == [f"tl:{app_id(name)}", "read"]), None))
    try:
        listing = session.invoke("app-actions", action["id"])
        value = listing.get("receipt", {}).get("update", {}).get("output", {}).get("value", {})
        parts = [json.dumps(value)]
        for record in value.get("records", []) if isinstance(value, dict) else []:
            if record.get("kind", "").startswith(f"tl.{app_id(name)}."):
                read = session.invoke("app-actions", action["id"], {"item": record["item"]})
                parts.append(json.dumps(read.get("receipt", {}).get("update", {}).get("output", {})))
    except Failed as error:
        # A directory id names one revision of the app's directory; once the app changed, the
        # action is listed again under the next one.
        if "unknown_action" in str(error):
            return None
        raise
    return "\n".join(parts)


def window_frame(session: Session, name: str) -> int | None:
    """The last frame the window of tilolang `name` presented, by its sequence number."""
    return next((f["frame_seq"] for f in session.frames() if f.get("app_id") == app_id(name)), None)


def holds(session: Session, name: str, text: str) -> bool | None:
    """Whether the running instance of `name` holds `text`; None while its listing moves."""
    content = read_content(session, name)
    return None if content is None else text in content


class Repl:
    """`tilolang repl` on the session's door, one entry at a time. Off a terminal the REPL
    echoes each line it reads after its prompt (`tl> entry`), so an entry's answer is what it
    prints between that echo and the echo of the empty line sent after it."""

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
        self.banner = self._read_until(lambda text: "\n" in text and text)

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

NOTE_TITLE = "Launch plan for Tide"
CHANNEL = "Tide release"
TASK_TITLE = "Write the Tide release notes"


def check_open(session: Session) -> dict:
    """The four apps opened at once, onto the canvas, each with a first frame, first try."""
    session.invoke("actions", "workspace.show-canvas")
    wait_for("canvas shown", lambda: session.state_dump()["chrome"].get("canvas", {}).get("shown"))
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


def check_canvas(session: Session) -> dict:
    """Every window placed on the canvas is on screen: its screen rect inside the canvas's
    frame, and its app's surface drawn there."""
    canvas = session.state_dump()["chrome"]["canvas"]
    fx, fy, fw, fh = canvas["frame"]
    placed = canvas.get("windows", [])
    groups = {w["group"]: w.get("app_id") for w in session.windows()}
    off = []
    for window in placed:
        x, y, w, h = window["screen"]
        if x < fx - 0.5 or y < fy - 0.5 or x + w > fx + fw + 0.5 or y + h > fy + fh + 0.5:
            off.append({"app": groups.get(window["group"]), "screen": window["screen"]})
    on_canvas = {groups.get(w["group"]) for w in placed}
    missing = {app_id(n) for n in TL_APPS} - on_canvas
    if missing:
        raise Failed(f"not placed on the canvas: {sorted(missing)}; placed {placed}")
    if off:
        raise Failed(f"placed off screen (the canvas's frame is {canvas['frame']}): {off}")
    return {"frame": canvas["frame"], "placed": len(placed)}


def check_repl(session: Session, out: Path) -> dict:
    """The door's REPL writes, finds, greps, adds a task the Tasks instance lists, and undoes."""
    repl = Repl(session, out)
    try:
        apps = re.search(r"apps: ([^(]*)", repl.banner)
        if not apps or not all(n in apps.group(1) for n in TL_APPS):
            raise Failed(f"the REPL does not reach the four apps: {repl.banner.strip()}")
        repl("let n = notes.new_note(notes.notes)")
        repl(f'notes.retitle(n, "{NOTE_TITLE}")')
        repl(f'chat.new_channel(chat.channels, "{CHANNEL}")')
        found = repl('find("launch plan")')
        if NOTE_TITLE not in found:
            raise Failed(f"find(\"launch plan\") did not return the note: {found!r}")
        in_notes = repl("grep(/Tide/, in: notes)")
        if NOTE_TITLE not in in_notes:
            raise Failed(f"grep over Notes did not return the note: {in_notes!r}")
        in_chat = repl("grep(/Tide/, in: chat)")
        if CHANNEL not in in_chat:
            raise Failed(f"grep over Chat did not return the channel: {in_chat!r}")
        drawn = window_frame(session, "tasks")
        added = repl(f'tasks.add(tasks.tasks, "{TASK_TITLE}")')
        if "1 change in tasks" not in added:
            raise Failed(f"tasks.add changed nothing: {added!r}")
        wait_for("the new task in the Tasks instance", lambda: holds(session, "tasks", TASK_TITLE))
        # The open Tasks window draws what its instance took in, with nothing else waking it.
        drawn = wait_for("a frame of the Tasks window showing the new task",
                         lambda: (window_frame(session, "tasks") or 0) > drawn and window_frame(session, "tasks"))
        undone = repl("undo")
        if "undone" not in undone:
            raise Failed(f"undo took nothing back: {undone!r}")
        wait_for("the task gone from the Tasks instance",
                 lambda: holds(session, "tasks", TASK_TITLE) is False)
        wait_for("a frame of the Tasks window without the undone task",
                 lambda: (window_frame(session, "tasks") or 0) > drawn)
        # The room admitted every change the apps and the REPL made: a change the sealed
        # channel refuses (the first, unnamed change of a tilolang app's record) is dropped
        # with only this line to say so, and its record is missing wherever the room is read.
        refused = [line[:400] for line in session.log_lines() if "change carries no claim" in line]
        if refused:
            raise Failed(f"the room refused {len(refused)} change(s) as carrying no claim: {refused[0]}")
        return {"find": found.strip(), "grep_notes": in_notes.strip().splitlines()[0],
                "grep_chat": in_chat.strip().splitlines()[0], "added": added.strip(), "undone": undone.strip()}
    finally:
        repl.close()
        (out / "repl-transcript.txt").write_text(repl.transcript)


def logical_resource(resource: str) -> str:
    """The app's own id of `resource`: an instance with no window (the windowless Tasks)
    publishes its resources under its instance's alias, `BOUND_RESOURCE_PREFIX` and the id in
    hex last, as `tilo_actions::selection::logical_resource_id` reads it."""
    if not resource.startswith(BOUND_RESOURCE_PREFIX):
        return resource
    try:
        return bytes.fromhex(resource.rsplit(":", 1)[-1]).decode()
    except ValueError:
        return resource


def check_ask(session: Session, model: TestOnlyFixedReplyModel) -> dict:
    """Asking an agent makes the tracker issue, opens no window and starts a task agent."""
    session.provision(model)
    row = wait_for("ask row ready for a new agent", lambda: next((
        a for a in session.actions(REQUEST)
        if a.get("provider") == "ask-agent" and "Settings" not in (a.get("subtitle") or "")), None),
        lambda: [a.get("subtitle") for a in session.actions(REQUEST) if a.get("provider") == "ask-agent"])
    before = sorted((w.get("app_id"), w.get("tab")) for w in session.windows())
    session.invoke(row["provider"], row["id"])

    def issue() -> str | None:
        """The issue's address: listed among the workspace's records under the request, or
        named in what its task agent was told."""
        listed = next((logical for a in session.actions(REQUEST)
                       if (logical := logical_resource(a.get("resource") or ""))
                       .startswith(TRACKER_ISSUES)), None)
        if listed:
            return listed
        told = re.search(re.escape(TRACKER_ISSUES) + r"[\w-]+", json.dumps(model.task_turns))
        return told.group(0) if told else None

    made = wait_for("tracker issue for the request", issue,
                    lambda: f"model errors {model.errors}\n{session.log_tail()}")
    wait_for("task agent asking the model", lambda: model.task_turns,
             lambda: f"model errors {model.errors}\n{session.log_tail()}")
    after = sorted((w.get("app_id"), w.get("tab")) for w in session.windows())
    if after != before:
        raise Failed(f"asking opened or closed windows: before {before}, after {after}")
    # The agent's change landed: the task its program added is in the running Tasks.
    wait_for("the task the agent added, in Tasks", lambda: holds(session, "tasks", ASKED_TASK),
             lambda: f"model errors {model.errors}; turns {len(model.task_turns)}; the program "
                     f"answered: {text_of(model.task_turns[-1][-1].get('content'))[:1500] if len(model.task_turns) > 1 else '-'}"
                     f"\n{session.log_tail()}")
    # The run holds and settles each turn's spend and reports its own end on the issue, and
    # only then does its task agent exit. Catches the agent's spend arguments not matching
    # Tasks' actions and its finish-run being refused as made for a task that had ended.
    exited = wait_for("the task agent ending with its task",
                      lambda: next((line for line in session.log_lines() if AGENT_EXITED in line), None),
                      lambda: session.log_tail())
    refused = [line for line in session.log_lines() if any(r in line for r in RUN_REFUSALS)]
    if refused:
        raise Failed("the run's own reports were refused:\n" + "\n".join(refused[:8]))
    if not exited.rstrip().endswith("exit status: 0"):
        raise Failed(f"the task agent did not end cleanly: {exited}")
    return {"issue": made, "task_turns": len(model.task_turns)}


def check_restart(session: Session) -> dict:
    """Started again on the same state: the four windows come back with frames and content."""
    session.stop()
    began = time.monotonic()
    session.start(install=False)
    wanted = {app_id(n) for n in TL_APPS}
    wait_for("every window back with a frame", lambda: wanted <= presented(session),
             lambda: f"presented {sorted(presented(session))}; windows {sorted(windows_by_app(session))}\n"
                     f"{session.log_tail()}")
    wait_for("the note back in Notes", lambda: holds(session, "notes", NOTE_TITLE))
    wait_for("the channel back in Chat", lambda: holds(session, "chat", CHANNEL))
    return {"seconds_to_content": round(time.monotonic() - began, 2)}


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

CHECKS = ["open", "canvas", "repl", "ask", "restart", "dev"]


def build_session(out: Path) -> Path:
    """The nested-session build of this tree, compiled on the arm builder."""
    target = out / "nested-session"
    subprocess.run(["bash", REPO / "dist/dev/nested-session-build.sh", target],
                   cwd=REPO, check=True, env={**os.environ, "TILO_NESTED_APPS": NATIVE})
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
    parser.add_argument("--out", type=Path, default=REPO / "target/session-e2e")
    parser.add_argument("--keep", action="store_true", help="leave the session running at the end")
    parser.add_argument("--only", help="comma list of checks to run (default: all, in order)")
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=True)
    for stale in out.glob("*.log"):
        stale.unlink()
    began = time.monotonic()
    build = build_session(out) if args.build else Path(
        os.environ.get("TILO_NESTED_SESSION_BUILD", REPO / "target/nested-session")).resolve()
    if not (build / "bin/shell").is_file():
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
