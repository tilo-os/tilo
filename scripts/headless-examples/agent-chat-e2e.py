#!/usr/bin/env python3
"""An ask in a fresh session reaches a model, and its reply appears in the chat.

Starts a headless nested Tilo session (dist/dev/nested-session.sh, lavapipe only,
never this machine's GPU) on a fresh state directory, and a scripted model beside
it: a loopback OpenAI-compatible server that answers the agent's loop turns from
what each request carries. Then, only through the semantic control surface
(tilo-control):

  provider  Settings' own actions add a local provider (the Ollama preset)
            pointing at the scripted model, choose its model and make it the
            default
  ask       the palette's "Ask an agent: ..." asks a new agent made for the task
            in the composer: the composer makes the issue in Tasks (which runs
            without a window), mints the task's agent and launches it
  run       the agent joins under its task's delegation, takes the run and plans
            with the scripted model: the model is asked at least once, with the
            loop's tools
  reply     the run ends and its reply is in the issue's thread: Tasks opens the
            issue as a chat and its read shows the agent's reply

Every link of that chain fails the script with what was seen instead, so a
broken link names itself. A PNG of the chat is written to OUT.

Usage:
  scripts/headless-examples/agent-chat-e2e.py [--no-build] [--out DIR] [--keep]

Without --no-build it builds the nested session first (nested-session.sh's own
build). TILO_NESTED_SESSION_BUILD names a prepared build to run instead.
"""

import argparse
import http.server
import itertools
import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SESSION = REPO / "dist/dev/nested-session.sh"
REPLY = "Reviewed Jane Doe's application: a strong CV, and a reply is drafted."
ASK = "Review Jane Doe's application and draft a reply."


class Failed(Exception):
    pass


def until(what: str, check, deadline: float = 90.0, step: float = 0.5):
    end = time.monotonic() + deadline
    seen = None
    while time.monotonic() < end:
        seen = check()
        if seen:
            return seen
        time.sleep(step)
    raise Failed(f"timed out waiting for {what}")


class ScriptedModel:
    """A loopback OpenAI-compatible model: every loop turn finishes the task
    with REPLY; any other request is answered with a small JSON answer."""

    def __init__(self):
        self.requests = []
        model = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_GET(self):
                body = json.dumps({"object": "list", "data": [{"id": "scripted", "object": "model"}]})
                self.send_response(200)
                self.send_header("content-type", "application/json")
                self.send_header("content-length", str(len(body)))
                self.end_headers()
                self.wfile.write(body.encode())

            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers.get("content-length", 0))) or b"{}")
                model.requests.append(body)
                tools = [tool["function"]["name"] for tool in body.get("tools") or []]
                self.send_response(200)
                self.send_header("content-type", "text/event-stream")
                self.end_headers()
                n = len(model.requests)

                def event(delta, finish=None):
                    chunk = {"id": f"c{n}", "object": "chat.completion.chunk", "created": 0,
                             "model": "scripted",
                             "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}
                    self.wfile.write(b"data: " + json.dumps(chunk).encode() + b"\n\n")

                if "plan" in tools:
                    arguments = json.dumps({"reply": REPLY, "done": True})
                    event({"role": "assistant", "tool_calls": [{"index": 0, "id": f"call{n}", "type": "function",
                           "function": {"name": "plan", "arguments": arguments}}]})
                    event({}, "tool_calls")
                else:
                    event({"role": "assistant", "content": json.dumps({"answer": REPLY})})
                    event({}, "stop")
                usage = {"id": f"c{n}", "object": "chat.completion.chunk", "created": 0, "model": "scripted",
                         "choices": [], "usage": {"prompt_tokens": 100, "completion_tokens": 10,
                                                  "total_tokens": 110}}
                self.wfile.write(b"data: " + json.dumps(usage).encode() + b"\n\ndata: [DONE]\n\n")
                self.wfile.flush()

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.port = self.server.server_address[1]
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def loop_turns(self):
        return [body for body in self.requests if body.get("tools")]


class Session:
    def __init__(self, state: Path, ready: dict, build: Path):
        self.state = state
        self.ready = ready
        self.control = build / "bin/tilo-control"

    def env(self) -> dict:
        env = dict(os.environ)
        env.update(HOME=str(self.state / "home"), XDG_CONFIG_HOME=str(self.state / "config"),
                   XDG_DATA_HOME=str(self.state / "data"), XDG_STATE_HOME=str(self.state / "state"),
                   XDG_CACHE_HOME=str(self.state / "cache"), XDG_RUNTIME_DIR=self.ready["runtime"],
                   TILO_TEST_CONTROL=self.ready["control"])
        return env

    def run(self, *args: str) -> str:
        result = subprocess.run([str(self.control), *args], text=True, capture_output=True, env=self.env())
        if result.returncode != 0:
            raise Failed(f"tilo-control {' '.join(args)}: {result.stderr.strip()[-1500:]}")
        return result.stdout

    def actions(self, query: str) -> list:
        listed = json.loads(self.run("actions", "list", query, "--json", "--programmatic", "--wait=8000"))
        return listed.get("actions", [])

    def invoke_app_action(self, query: str, action: str, args: dict | None = None, record_part: str = ""):
        offered = [row for row in self.actions(query)
                   if row["provider"] == "app-actions" and row["id"].endswith(action)
                   and record_part in row["id"]]
        # A live window's own row first; the directory's row of the same
        # action reaches it too.
        rows = sorted(offered, key=lambda row: row["id"].startswith("directory"))
        if not rows:
            raise Failed(f"no {action} is offered (asked for {query!r})")
        argv = ["actions", "invoke", rows[0]["provider"], rows[0]["id"], "--wait=20000"]
        if args is not None:
            argv += ["--args-json", json.dumps(args)]
        answer = json.loads(self.run(*argv))
        state = answer.get("receipt", {}).get("update", {}).get("state")
        if state != "succeeded":
            raise Failed(f"{action} did not succeed: {answer}")
        return answer

    def log(self) -> str:
        return (self.state / "logs/shell.log").read_text(errors="replace")


def _issue_rows(session: "Session", action: str) -> list:
    """The `action` rows Tasks publishes on its issues (items of the
    tracker, named by the issue's address)."""
    found = []
    for row in session.actions(action):
        if row["provider"] != "app-actions" or not row["id"].endswith(action):
            continue
        resource = row["id"].split("\x1f")[-2] if "\x1f" in row["id"] else ""
        try:
            named = bytes.fromhex(resource.rsplit(":", 1)[-1]).decode()
        except ValueError:
            continue
        if "/issues/" in named:
            found.append(row)
    return found


def _run_finished(session: "Session") -> bool:
    """Whether the asked issue's run ended with its report on the issue: its
    run is no longer going, so the issue offers to run its agent again (its
    agent's `finish-run`, which posts the reply in the thread, landed)."""
    return bool(_issue_rows(session, "approve-run"))


def _pressed(session: "Session") -> bool:
    """Press the composer's Open as chat; whether a Tasks window came up."""
    session.run("click", "agent-quick-open-chat")
    time.sleep(2)
    return "so.tilo.tasks" in session.run("windows")


def start_session(state: Path, out: Path, build: Path | None) -> tuple[subprocess.Popen, dict]:
    ready_file = out / "ready.json"
    ready_file.unlink(missing_ok=True)
    argv = [str(SESSION), "--headless", "--software", "--state", str(state), "--ready", str(ready_file)]
    env = dict(os.environ)
    if build is not None:
        argv.insert(1, "--no-build")
        env["TILO_NESTED_SESSION_BUILD"] = str(build)
    env.setdefault("RUST_LOG", "warn,shell::shell::overlay_host=info,tilo_action_broker=info")
    proc = subprocess.Popen(argv, stdout=open(out / "session.log", "w"), stderr=subprocess.STDOUT,
                            start_new_session=True, env=env, cwd=REPO)
    ready = until("the session's control socket",
                  lambda: ready_file.exists() and ready_file.stat().st_size > 0
                  and json.loads(ready_file.read_text()), deadline=600)
    return proc, ready


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--out", type=Path)
    parser.add_argument("--keep", action="store_true")
    options = parser.parse_args()
    out = options.out or Path(tempfile.mkdtemp(prefix="agent-chat-e2e-", dir=os.environ.get("TMPDIR")))
    out.mkdir(parents=True, exist_ok=True)
    state = out / "state"
    shutil.rmtree(state, ignore_errors=True)
    build = os.environ.get("TILO_NESTED_SESSION_BUILD")
    build = Path(build) if build else (REPO / "target/nested-session" if options.no_build else None)
    model = ScriptedModel()
    proc = None
    try:
        proc, ready = start_session(state, out, build)
        session = Session(state, ready, build or REPO / "target/nested-session")
        # provider: Settings' own actions, as a person adds one.
        session.run("actions", "invoke", "settings-pages", "page:agent_models", "--wait")
        until("Settings' provider actions", lambda: any(
            row["id"].endswith("add-provider") for row in session.actions("Add provider")))
        session.invoke_app_action("Add provider", "add-provider", {"preset": "ollama"})
        until("the new provider's row", lambda: any(
            row["id"].endswith("set-provider-details") for row in session.actions("Ollama")))
        session.invoke_app_action("Ollama", "set-provider-details", {
            "label": "Scripted", "endpoint": f"http://127.0.0.1:{model.port}/v1", "model": "scripted"})
        session.invoke_app_action("Use model", "set-provider-model", {"model": "scripted"})
        session.invoke_app_action("Use by default", "set-default-provider")
        # ask: the palette's row asks in the composer, once it can: until the
        # model setting reaches the session the row opens Settings instead.
        until("the palette to ask a new agent", lambda: any(
            row["provider"] == "ask-agent" and (row.get("subtitle") or "").startswith("A task for")
            for row in session.actions(ASK)))
        # The palette's ask opens the composer about what is focused once the
        # focused window answers its selection; that answer is sometimes lost
        # (the window's "handoff: no instance route to answer on"), which is
        # its own bug: asked again then, and said.
        for attempt in range(3):
            session.run("actions", "invoke", "ask-agent", f"ask:{ASK}", "--wait=10000")
            try:
                until("the palette's ask to open the composer",
                      lambda: "agent-quick-editor" in session.run("list-targets"), deadline=15)
                break
            except Failed:
                print(f"agent-chat-e2e: the palette's ask opened no composer (try {attempt + 1})",
                      file=sys.stderr)
        else:
            raise Failed("the palette's ask never opened the composer")
        # run: the task's agent plans with the model.
        try:
            until("the agent's first loop turn at the model", model.loop_turns, deadline=120)
        except Failed:
            refusals = [line for line in session.log().splitlines()
                        if "ended without its result" in line or "found no app to take it" in line
                        or "task agent join rejected" in line or "could not be launched" in line]
            raise Failed("no loop turn reached the model; the session said: "
                         + (" | ".join(refusals[-5:]) or "nothing"))
        # reply: "Open as chat" in the composer, which follows the run, opens
        # the run's issue as a chat in one window of its own (the desktop's
        # `open` of the chat's address), and the thread there holds the reply.
        until("the composer's Open as chat",
              lambda: "agent-quick-open-chat" in session.run("list-targets"))
        until("a Tasks window from Open as chat", lambda: _pressed(session), deadline=60)
        windows = session.run("windows")
        if windows.count("so.tilo.tasks") != 1:
            raise Failed(f"Open as chat opened {windows.count('so.tilo.tasks')} Tasks windows: {windows}")
        session.run("snapshot", str(out / "chat.png"))
        # The chat shows the issue's thread; Tasks' read of its tracker (from
        # a window of the app, which publishes the read) holds the same.
        held = until("the run's end and reply on the issue", lambda: _run_finished(session), deadline=60)
        print(json.dumps({"loop_turns": len(model.loop_turns()), "run_ended_on_issue": held,
                          "out": str(out)}, indent=1))
        return 0
    except Failed as failure:
        print(f"agent-chat-e2e: {failure}", file=sys.stderr)
        if proc is not None:
            try:
                session.run("snapshot", str(out / "failure.png"))
            except (Failed, NameError):
                pass
        return 1
    finally:
        model.server.shutdown()
        if proc is not None:
            os.killpg(proc.pid, signal.SIGTERM)
            proc.wait(timeout=60)
        if not options.keep:
            shutil.rmtree(state, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
