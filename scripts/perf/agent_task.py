#!/usr/bin/env python3
"""An agent task, end to end: a scripted model drives Notes through the agent door.

The task is the one a person gives an agent: "Create a note titled T with
the body B in Notes, then find it by searching and read it back." It runs
the way an agent in a terminal inside a Tilo login (Claude Code, Codex)
runs it, with the model replaced so the number measures Tilo and needs no
key:

  agent loop  --HTTP, Anthropic Messages streaming-->  fake model (loopback)
  agent loop  --MCP stdio-->  tilo-runtime-mcp --connect
              --agent.sock-->  compositor (as the admitted agent) -> Notes

`tilo-runtime-mcp --connect` is the program `tilo agent connect claude`
registers. Inside the login it relays to the session's agent door, so the
agent is admitted by the person (here: the `permission-prompts` decision on
the control socket, as the owner answers remotely), is an agent member of
the workspace, is granted Notes like Settings → Agents grants it, and every
call passes the broker's gate as that agent. Its tools are the door's own
path-addressed ones (`tilo_find`, `tilo_do`, `tilo_ls`, `tilo_read`), and
the fake model is offered exactly what the door's `tools/list` answers.

The fake model is a loopback HTTP server that answers each turn with a
streamed `tool_use` (server-sent events, as the Messages API streams them).
Its policy reads the previous tool results the way a perfect model would and
picks the next call from what Tilo answered; it knows no path in advance.
`--think-ms` delays each turn's first content block and `--tokens-per-s`
paces the stream, so a run can model a real provider; the scheduled lane
runs them at 0, and the model's share is then only the fake server's
loopback overhead, reported apart.

The steps, one tool call per turn because each needs the last one's result:

  discover  tilo_find "new note" in notes: the action that makes a note
  create    tilo_do it with the title
  locate    tilo_find the title: the new note (or its body's append action
            straight away), then tilo_ls of the note for its body
            ("locate.inspect")
  write     tilo_do the body's append-paragraph with the body text
  read      tilo_read the note: the body text is in it

then a final text turn. Each step is timed from the request written to the
client's stdin to its whole answer read back: the relay, the door, the
broker, the app applying the action and the answer. A result that says the
note is not there yet (`nothing matches`, a path that does not resolve, a
read without the text) is read and the step asked again, as a model would,
named `<step>.retry` and counted to its step; a run that needs more than
MAX_RETRIES of them fails.

Records (tools/perf/record.py), one sample per repetition:

  agent.task.notes.wall          the task end to end, ms
  agent.task.notes.tilo          the tool calls together, ms
  agent.task.notes.model         the model turns together (fake), ms
  agent.task.notes.client        the agent loop's own work, ms
  agent.task.notes.turns         model turns the task took
  agent.task.notes.retries       tool calls asked again
  agent.task.notes.step.<step>   one step's tool calls, ms

The desktop is `crates/session/shell/examples/agent_task_desktop.rs`: a
signed-in person with one empty Notes notebook, restored from a seeded
profile, serving its agent door in the profile's runtime directory. The
compositor, its profile, its sockets and the client are removed on exit,
whatever happens. Against a running desktop (`--socket`, a device's
admin-control.sock, with its `agent.sock` beside it) nothing is seeded, and
the task never touches the notebook a person already has: the run first
creates a workspace of its own (a new room) and opens Notes there
(`scratch_notes.py`), so the agent is admitted to that workspace and its
`notes` is that room's notebook; the agent is removed again at the end.
"""

from __future__ import annotations

import argparse
import contextlib
import http.client
import http.server
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable, Iterator

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "tools" / "perf"))
sys.path.insert(0, str(Path(__file__).resolve().parent))
import record  # noqa: E402
from agent_rtt import (  # noqa: E402
    MCP_PROTOCOL, BenchError, Connection, answer_of, receipt_state, request, spawned)
from scratch_notes import NOTES_APP, ScratchRoomError, open_scratch_notes  # noqa: E402

# Generous: each answer leaves as soon as its invocation settles.
WAIT_MS = 5000
STEPS = ("discover", "create", "locate", "write", "read")
# The client the person admits; the door names the agent after it.
CLIENT_NAME = "Agent task bench"
AGENT = "agent-task-bench"
GRANT_ID = "agent-task-bench-notes"
# The fake model's name, echoed in records.
MODEL_NAME = "tilo-scripted-notes-task"
# Rough characters per token, to pace a streamed answer at --tokens-per-s.
CHARS_PER_TOKEN = 4
# How long one door call may take before the run fails.
CALL_TIMEOUT_S = 60.0


# --- the fake model -------------------------------------------------------------

class PolicyError(RuntimeError):
    """The scripted model found nothing to do with what Tilo answered."""


@dataclass
class Result:
    """A tool's answer as the model reads it: the door answers text."""

    text: str
    failed: bool


def conversation_calls(messages: list[dict[str, Any]]) -> list[tuple[dict[str, Any], Result]]:
    """Every (tool_use block, tool result) pair of the conversation, in order."""
    uses: dict[str, dict[str, Any]] = {}
    pairs: list[tuple[dict[str, Any], Result]] = []
    for message in messages:
        content = message.get("content")
        if not isinstance(content, list):
            continue
        for block in content:
            if block.get("type") == "tool_use":
                uses[block["id"]] = block
            elif block.get("type") == "tool_result":
                use = uses.get(block.get("tool_use_id", ""))
                if use is None:
                    raise PolicyError("a tool_result answers no tool_use")
                text = block.get("content")
                if isinstance(text, list):
                    text = "".join(part.get("text", "") for part in text)
                pairs.append((use, Result(str(text or ""), bool(block.get("is_error")))))
    return pairs


@dataclass(frozen=True)
class Hit:
    """One line of a door listing: `path#action(params)  label`, or `path  kind`."""

    path: str
    action: str | None
    params: tuple[str, ...]
    label: str


HIT = re.compile(r"^(?P<path>(?:\\.|[^#\\])*?)(?:#(?P<action>[\w.:@-]+)(?:\((?P<params>[^)]*)\))?)?"
                 r"(?:  +(?P<label>.*))?$")


def hits_of(text: str) -> list[Hit]:
    """The door's lines, read the way a model reads them."""
    found = []
    for line in text.splitlines():
        match = HIT.match(line.strip())
        if not match or line.startswith(("… ", "nothing matches", "apps")):
            continue
        params = tuple(part.strip() for part in (match["params"] or "").split(",") if part.strip())
        found.append(Hit(match["path"].strip(), match["action"], params, (match["label"] or "").strip()))
    return found


def param_name(hit: Hit, prefer: str) -> str:
    """The parameter to pass the text in: `prefer` when the action takes it,
    else its first (`title?`, `kind:a|b` name only)."""
    names = [re.split(r"[?:]", param, maxsplit=1)[0] for param in hit.params]
    if prefer in names:
        return prefer
    if not names:
        raise PolicyError(f"{hit.path}#{hit.action} takes no parameter")
    return names[0]


def describe(text: str, limit: int = 1500) -> str:
    return text if len(text) <= limit else text[:limit] + "…"


@dataclass
class NotesTask:
    """The scripted model of the Notes task: one tool call per turn, then a reply.

    The note appears in the door's listings a moment after it is created and
    its text a moment after it is written; until then the model asks again,
    as a model reading "nothing matches" would, at most MAX_RETRIES times a
    task. Those calls are named `<step>.retry` and count to their step.
    """

    title: str
    body: str
    # The step of every tool call this model issued, in order.
    steps: list[str] = field(default_factory=list)
    retries: int = 0
    # What the conversation established: the note's path and its body's.
    note: str | None = None
    append: Hit | None = None
    # The last answer that sent the model to ask again.
    last_miss: str = ""

    MAX_RETRIES = 100

    def prompt(self) -> str:
        return (f"Create a note titled {self.title!r} with the body {self.body!r} in Notes, "
                f"then find it by searching for its title and read it back.")

    def respond(self, messages: list[dict[str, Any]]) -> list[dict[str, Any]]:
        """The next turn's content blocks for this conversation."""
        done = conversation_calls(messages)
        if not done:
            return [self.call("discover", "tilo_find", {"query": "new note", "app": "notes"})]
        last = done[-1][1]
        step = self.steps[len(done) - 1]
        base = step.split(".", 1)[0]
        if base == "discover":
            found = [hit for hit in hits_of(last.text) if hit.action == "new-note"]
            if last.failed or not found:
                raise PolicyError(f"no Notes new-note in what tilo_find answered: {describe(last.text)}")
            hit = found[0]
            return [self.call("create", "tilo_do", {"path": f"{hit.path}#{hit.action}",
                                                    "args": {param_name(hit, "title"): self.title}})]
        if base == "create":
            succeeded(last, "create")
            return [self.locate()]
        if step == "locate.inspect":
            return self.after_inspect(last)
        if base == "locate":
            return self.after_locate(last)
        if base == "write":
            if last.failed and stale(last):
                # The path moved under the agent: find the note again.
                return [self.locate(retry=True)]
            succeeded(last, "write")
            return [self.read()]
        if base == "read":
            if self.body in last.text and not last.failed:
                return [{"type": "text", "text": f"Created {self.title!r}; it reads back."}]
            self.last_miss = last.text
            return [self.read(retry=True)]
        raise PolicyError(f"the task has no step after {step}")

    def after_locate(self, last: Result) -> list[dict[str, Any]]:
        hits = [hit for hit in hits_of(last.text) if self.title.lower() in hit.path.lower()]
        appends = [hit for hit in hits if hit.action == "append-paragraph"]
        if not last.failed and len(appends) == 1:
            self.append = appends[0]
            self.note = parent_of(appends[0].path, self.title)
            return [self.write()]
        # The note itself, or actions on it, but not its body's: look inside it.
        notes = sorted({hit.path for hit in hits if hit.path.lower().endswith(self.title.lower())})
        if not last.failed and len(notes) == 1:
            self.note = notes[0]
            return [self.call("locate.inspect", "tilo_ls", {"path": self.note})]
        if len(appends) > 1 or len(notes) > 1:
            raise PolicyError(f"more than one note titled {self.title!r}: {describe(last.text)}")
        self.last_miss = last.text
        return [self.locate(retry=True)]

    def after_inspect(self, last: Result) -> list[dict[str, Any]]:
        """The note's own listing: its body is the child with append-paragraph,
        or the child named body."""
        assert self.note is not None
        children = [hit for hit in hits_of(last.text) if hit.action is None]
        bodies = [hit for hit in children if hit.path.rstrip("/").lower() == "body"]
        if last.failed or len(bodies) != 1:
            self.last_miss = last.text
            return [self.locate(retry=True)]
        body = f"{self.note}/{bodies[0].path.rstrip('/')}"
        self.append = Hit(body, "append-paragraph", ("text",), "")
        return [self.write()]

    def locate(self, retry: bool = False) -> dict[str, Any]:
        return self.call("locate.retry" if retry else "locate", "tilo_find",
                         {"query": self.title, "app": "notes"})

    def write(self) -> dict[str, Any]:
        assert self.append is not None
        name = "write.retry" if any(step.startswith("write") for step in self.steps) else "write"
        return self.call(name, "tilo_do", {"path": f"{self.append.path}#append-paragraph",
                                           "args": {param_name(self.append, "text"): self.body}})

    def read(self, retry: bool = False) -> dict[str, Any]:
        assert self.note is not None
        return self.call("read.retry" if retry else "read", "tilo_read", {"path": self.note})

    def call(self, step: str, name: str, arguments: dict[str, Any]) -> dict[str, Any]:
        if step.endswith(".retry"):
            if self.retries >= self.MAX_RETRIES:
                raise PolicyError(f"{step.split('.')[0]} still not done after {self.retries} retries; "
                                  f"last answer: {describe(self.last_miss)}")
            self.retries += 1
        self.steps.append(step)
        return tool(name, arguments)


def parent_of(path: str, title: str) -> str:
    """The note's path above a child of it (`notes/Notebook/T/Body` -> `…/T`)."""
    at = path.lower().rfind(title.lower())
    if at < 0:
        raise PolicyError(f"{path!r} is not inside the note {title!r}")
    return path[:at + len(title)]


def succeeded(result: Result, what: str) -> None:
    if result.failed or not result.text.startswith("ok"):
        raise PolicyError(f"{what} did not succeed: {describe(result.text, 600)}")


def stale(result: Result) -> bool:
    """Whether a failure says the path no longer names anything there."""
    text = result.text.lower()
    return any(phrase in text for phrase in ("list it again", "no longer", "not found", "nothing named",
                                             "unknown_action", "tilo_ls"))


_tool_ids = iter(range(1, 1 << 62))


def tool(name: str, arguments: dict[str, Any]) -> dict[str, Any]:
    return {"type": "tool_use", "id": f"toolu_{next(_tool_ids):08d}", "name": name,
            "input": arguments}


def sse_events(blocks: list[dict[str, Any]], usage_in: int) -> Iterator[tuple[str, dict[str, Any] | None]]:
    """The Messages stream of one assistant turn: (event, data), with
    `("pace", {"chars": n})` markers where a paced stream waits."""
    yield "message_start", {"type": "message_start", "message": {
        "id": f"msg_{next(_tool_ids):08d}", "type": "message", "role": "assistant",
        "model": MODEL_NAME, "content": [], "stop_reason": None,
        "usage": {"input_tokens": usage_in, "output_tokens": 0}}}
    yield "think", None
    output_chars = 0
    for index, block in enumerate(blocks):
        if block["type"] == "tool_use":
            start = {"type": "tool_use", "id": block["id"], "name": block["name"], "input": {}}
            text = json.dumps(block["input"], separators=(",", ":"))
            delta = "input_json_delta", "partial_json"
        else:
            start = {"type": "text", "text": ""}
            text = block["text"]
            delta = "text_delta", "text"
        yield "content_block_start", {"type": "content_block_start", "index": index,
                                      "content_block": start}
        # Pieces of a few tokens each, as providers stream.
        for at in range(0, len(text), CHARS_PER_TOKEN * 8):
            piece = text[at:at + CHARS_PER_TOKEN * 8]
            yield "pace", {"chars": len(piece)}
            yield "content_block_delta", {"type": "content_block_delta", "index": index,
                                          "delta": {"type": delta[0], delta[1]: piece}}
        output_chars += len(text)
        yield "content_block_stop", {"type": "content_block_stop", "index": index}
    stop = "tool_use" if any(b["type"] == "tool_use" for b in blocks) else "end_turn"
    yield "message_delta", {"type": "message_delta", "delta": {"stop_reason": stop},
                            "usage": {"output_tokens": max(1, output_chars // CHARS_PER_TOKEN)}}
    yield "message_stop", {"type": "message_stop"}


class FakeModel:
    """A loopback Anthropic Messages endpoint that streams a scripted policy's turns."""

    def __init__(self, respond: Callable[[list[dict[str, Any]]], list[dict[str, Any]]],
                 think_ms: float = 0.0, tokens_per_s: float = 0.0):
        self.respond = respond
        self.think_s = think_ms / 1000
        self.char_s = (1 / (tokens_per_s * CHARS_PER_TOKEN)) if tokens_per_s > 0 else 0.0
        self.errors: list[str] = []
        model = self

        class Handler(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *_args: Any) -> None:
                pass

            def do_POST(self) -> None:  # noqa: N802 (http.server's name)
                length = int(self.headers.get("content-length", "0"))
                body = json.loads(self.rfile.read(length))
                try:
                    if self.path != "/v1/messages" or not body.get("stream"):
                        raise PolicyError(f"unexpected request {self.path} stream={body.get('stream')}")
                    blocks = model.respond(body["messages"])
                except PolicyError as error:
                    model.errors.append(str(error))
                    payload = json.dumps({"type": "error", "error": {
                        "type": "invalid_request_error", "message": str(error)}}).encode()
                    self.send_response(400)
                    self.send_header("content-type", "application/json")
                    self.send_header("content-length", str(len(payload)))
                    self.end_headers()
                    self.wfile.write(payload)
                    return
                self.send_response(200)
                self.send_header("content-type", "text/event-stream")
                self.send_header("transfer-encoding", "chunked")
                self.end_headers()
                usage_in = len(json.dumps(body["messages"])) // CHARS_PER_TOKEN
                for event, data in sse_events(blocks, usage_in):
                    if event == "think":
                        if model.think_s:
                            time.sleep(model.think_s)
                        continue
                    if event == "pace":
                        if model.char_s and data:
                            time.sleep(model.char_s * data["chars"])
                        continue
                    chunk = f"event: {event}\ndata: {json.dumps(data)}\n\n".encode()
                    self.wfile.write(f"{len(chunk):x}\r\n".encode() + chunk + b"\r\n")
                    self.wfile.flush()
                self.wfile.write(b"0\r\n\r\n")
                self.wfile.flush()

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    @property
    def port(self) -> int:
        return self.server.server_address[1]

    def close(self) -> None:
        self.server.shutdown()
        self.server.server_close()


# --- the agent loop -------------------------------------------------------------

@dataclass
class ToolCall:
    step: str
    name: str
    ms: float
    response_bytes: int


@dataclass
class TaskRun:
    turns: int = 0
    wall_ms: float = 0.0
    model_ms: float = 0.0
    model_first_byte_ms: list[float] = field(default_factory=list)
    calls: list[ToolCall] = field(default_factory=list)

    @property
    def tilo_ms(self) -> float:
        return sum(call.ms for call in self.calls)

    @property
    def client_ms(self) -> float:
        return self.wall_ms - self.model_ms - self.tilo_ms


class ModelClient:
    """Streams one Messages turn over a kept-alive loopback connection."""

    def __init__(self, port: int, tools: list[dict[str, Any]]):
        self.connection = http.client.HTTPConnection("127.0.0.1", port, timeout=120)
        # As the Messages API takes them: an MCP tool's inputSchema is its input_schema.
        self.tools = [{"name": t["name"], "description": t.get("description", ""),
                       "input_schema": t.get("inputSchema", {"type": "object"})} for t in tools]

    def close(self) -> None:
        self.connection.close()

    def turn(self, messages: list[dict[str, Any]]) -> tuple[list[dict[str, Any]], str, float]:
        """(content blocks, stop reason, ms to the first streamed byte)."""
        body = json.dumps({"model": MODEL_NAME, "max_tokens": 1024, "stream": True,
                           "tools": self.tools, "messages": messages})
        start = time.perf_counter_ns()
        self.connection.request("POST", "/v1/messages", body=body,
                                headers={"content-type": "application/json",
                                         "anthropic-version": "2023-06-01"})
        response = self.connection.getresponse()
        if response.status != 200:
            detail = response.read().decode(errors="replace")
            raise BenchError(f"the model answered {response.status}: {detail[:2000]}")
        first_byte: float | None = None
        blocks: dict[int, dict[str, Any]] = {}
        partial: dict[int, str] = {}
        stop = ""
        event = ""
        while True:
            line = response.readline()
            if first_byte is None:
                first_byte = (time.perf_counter_ns() - start) / 1e6
            if not line:
                break
            text = line.decode().rstrip("\n")
            if text.startswith("event: "):
                event = text[7:]
            elif text.startswith("data: "):
                data = json.loads(text[6:])
                if event == "content_block_start":
                    blocks[data["index"]] = dict(data["content_block"])
                    partial[data["index"]] = ""
                elif event == "content_block_delta":
                    delta = data["delta"]
                    partial[data["index"]] += delta.get("partial_json", delta.get("text", ""))
                elif event == "content_block_stop":
                    block = blocks[data["index"]]
                    if block["type"] == "tool_use":
                        block["input"] = json.loads(partial[data["index"]] or "{}")
                    else:
                        block["text"] = partial[data["index"]]
                elif event == "message_delta":
                    stop = data["delta"].get("stop_reason") or ""
                elif event == "message_stop":
                    break
        response.read()
        return [blocks[index] for index in sorted(blocks)], stop, first_byte or 0.0


class Door:
    """`tilo-runtime-mcp --connect`, spoken to over its stdio as an MCP client does."""

    def __init__(self, process: subprocess.Popen):
        self.process = process
        self.next_id = 0
        self.lines: list[bytes] = []
        self.ready = threading.Condition()
        self.closed = False
        threading.Thread(target=self._read, daemon=True).start()

    @classmethod
    def start(cls, binary: Path, env: dict[str, str], log: Path | None) -> "Door":
        if not binary.is_file() or not os.access(binary, os.X_OK):
            raise BenchError(f"no tilo-runtime-mcp at {binary}")
        stderr = log.open("ab") if log else subprocess.DEVNULL
        try:
            process = subprocess.Popen([str(binary), "--connect"], env=env, stdin=subprocess.PIPE,
                                       stdout=subprocess.PIPE, stderr=stderr)
        finally:
            if log:
                stderr.close()
        return cls(process)

    def _read(self) -> None:
        assert self.process.stdout is not None
        for line in iter(self.process.stdout.readline, b""):
            with self.ready:
                self.lines.append(line)
                self.ready.notify_all()
        with self.ready:
            self.closed = True
            self.ready.notify_all()

    def send(self, message: dict[str, Any]) -> None:
        assert self.process.stdin is not None
        try:
            self.process.stdin.write(json.dumps(message, separators=(",", ":")).encode() + b"\n")
            self.process.stdin.flush()
        except OSError as error:
            raise BenchError(f"tilo-runtime-mcp stopped reading: {error}") from error

    def reply(self, ident: int, timeout: float = CALL_TIMEOUT_S) -> tuple[dict[str, Any], int]:
        """The answer to request `ident` (notifications before it are skipped), and its size."""
        deadline = time.monotonic() + timeout
        with self.ready:
            while True:
                while self.lines:
                    line = self.lines.pop(0)
                    message = json.loads(line)
                    if message.get("id") == ident:
                        return message, len(line)
                if self.closed:
                    raise BenchError(f"tilo-runtime-mcp closed (exit {self.process.poll()}) "
                                     f"before answering request {ident}")
                left = deadline - time.monotonic()
                if left <= 0:
                    raise BenchError(f"tilo-runtime-mcp did not answer request {ident} in {timeout:.0f} s")
                self.ready.wait(left)

    def request(self, method: str, params: dict[str, Any] | None = None,
                timeout: float = CALL_TIMEOUT_S) -> tuple[dict[str, Any], int, int]:
        """(reply, its size, nanoseconds from the request written to the reply read)."""
        self.next_id += 1
        ident = self.next_id
        message: dict[str, Any] = {"jsonrpc": "2.0", "id": ident, "method": method}
        if params is not None:
            message["params"] = params
        start = time.perf_counter_ns()
        self.send(message)
        reply, size = self.reply(ident, timeout)
        return reply, size, time.perf_counter_ns() - start

    def call(self, name: str, arguments: dict[str, Any]) -> tuple[Result, int, int]:
        reply, size, elapsed = self.request("tools/call", {"name": name, "arguments": arguments})
        if "error" in reply:
            raise BenchError(f"{name} answered a protocol error: {json.dumps(reply['error'])[:600]}")
        result = reply.get("result") or {}
        text = "".join(part.get("text", "") for part in result.get("content", [])
                       if isinstance(part, dict))
        return Result(text, bool(result.get("isError"))), size, elapsed

    def close(self) -> None:
        if self.process.stdin is not None:
            with contextlib.suppress(OSError):
                self.process.stdin.close()
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()
        if self.process.stdout is not None:
            self.process.stdout.close()


def run_task(model: ModelClient, door: Door, task: NotesTask, max_turns: int = 160) -> TaskRun:
    """One task: model turns and tool calls until the model ends its turn."""
    run = TaskRun()
    messages: list[dict[str, Any]] = [{"role": "user", "content": task.prompt()}]
    start = time.perf_counter_ns()
    while True:
        if run.turns >= max_turns:
            raise BenchError(f"the task took more than {max_turns} turns")
        turn_start = time.perf_counter_ns()
        blocks, stop, first_byte = model.turn(messages)
        run.model_ms += (time.perf_counter_ns() - turn_start) / 1e6
        run.model_first_byte_ms.append(first_byte)
        run.turns += 1
        messages.append({"role": "assistant", "content": blocks})
        uses = [block for block in blocks if block["type"] == "tool_use"]
        if stop != "tool_use" or not uses:
            break
        results = []
        for block in uses:
            step = task.steps[len(run.calls)]
            result, size, elapsed = door.call(block["name"], block["input"])
            run.calls.append(ToolCall(step, block["name"], elapsed / 1e6, size))
            results.append({"type": "tool_result", "tool_use_id": block["id"],
                            "content": result.text, "is_error": result.failed})
        messages.append({"role": "user", "content": results})
    run.wall_ms = (time.perf_counter_ns() - start) / 1e6
    firsts = [call.step for call in run.calls if "." not in call.step]
    if firsts != list(STEPS):
        raise BenchError(f"the task ran {[c.step for c in run.calls]}, not {list(STEPS)}")
    return run


# --- the desktop ----------------------------------------------------------------

def control(conn: Connection, payload: dict[str, Any], what: str) -> Any:
    _, line = conn.round_trip(request(payload))
    return answer_of(line, what)


def waiting_prompt(conn: Connection, timeout: float) -> str:
    """The agent admission the person is asked, as its decision rows name it."""
    deadline = time.monotonic() + timeout
    while True:
        listed = control(conn, {"op": "actions_enumerate", "query": "permission-prompts",
                                "limit": 2000}, "list the person's prompts")
        for row in listed.get("actions", []) if isinstance(listed, dict) else []:
            ident = str(row.get("id", ""))
            if row.get("provider") == "permission-prompts" and ident.startswith("deny@"):
                return ident.removeprefix("deny@")
        if time.monotonic() >= deadline:
            raise BenchError(f"the person was never asked to admit the agent within {timeout:.0f} s")
        time.sleep(0.1)


def admit(conn: Connection, door: Door, timeout: float) -> list[dict[str, Any]]:
    """Connect as an agent, have the person admit it and grant it Notes; the door's tools."""
    door.next_id += 1
    ident = door.next_id
    door.send({"jsonrpc": "2.0", "id": ident, "method": "initialize", "params": {
        "protocolVersion": MCP_PROTOCOL, "capabilities": {},
        "clientInfo": {"name": CLIENT_NAME, "version": "1"}}})
    prompt = waiting_prompt(conn, timeout)
    answer = control(conn, {"op": "actions_invoke", "provider": "permission-prompts",
                            "id": f"allow-once@{prompt}", "args": {"confirm": True},
                            "wait_ms": WAIT_MS}, "admit the agent")
    state = receipt_state(answer)
    if state not in (None, "succeeded"):
        raise BenchError(f"admitting the agent ended {state}: {json.dumps(answer)[:400]}")
    initialized, _ = door.reply(ident, timeout)
    if "error" in initialized or not isinstance(initialized.get("result", {}).get("serverInfo"), dict):
        raise BenchError(f"the door did not serve the admitted agent: {json.dumps(initialized)[:600]}")
    door.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
    # The agent holds no role of its own: the person grants it Notes.
    control(conn, {"op": "allow", "subject": f"agent:{AGENT}", "node": f"app:{NOTES_APP}",
                   "kinds": ["read", "undoable"], "answer": "allow", "id": GRANT_ID},
            "grant the agent Notes")
    listed, _, _ = door.request("tools/list")
    tools = (listed.get("result") or {}).get("tools")
    names = sorted(t.get("name") for t in tools or [])
    if not {"tilo_find", "tilo_do", "tilo_ls", "tilo_read"} <= set(names):
        raise BenchError(f"the door offers {names}, not the agent tools")
    return tools


def wait_for_notes(door: Door, conn: Connection, timeout: float, diagnostics: Path | None) -> None:
    """Until the agent finds Notes' new-note; fail with what the desktop showed."""
    deadline = time.monotonic() + timeout
    text = ""
    while time.monotonic() < deadline:
        result, _, _ = door.call("tilo_find", {"query": "new note", "app": "notes"})
        text = result.text
        if any(hit.action == "new-note" for hit in hits_of(text)):
            return
        time.sleep(0.2)
    seen = describe_desktop(conn, diagnostics)
    raise BenchError(f"the agent never found Notes' new-note within {timeout:.0f} s; "
                     f"tilo_find answered: {describe(text)}\n{seen}")


def open_own_notebook(conn: Connection, name: str, timeout: float) -> None:
    """A workspace of the run's own, in front, with Notes open in it."""
    def state() -> dict[str, Any]:
        reply = control(conn, {"op": "state_dump"}, "state_dump")
        return reply.get("state", reply) if isinstance(reply, dict) else {}

    def invoke(provider: str, action: str, args: dict[str, Any]) -> Any:
        answer = control(conn, {"op": "actions_invoke", "provider": provider, "id": action,
                                "args": args, "wait_ms": WAIT_MS}, f"{provider} {action}")
        if receipt_state(answer) != "succeeded":
            raise BenchError(f"{provider} {action} did not succeed: {json.dumps(answer)[:400]}")
        return answer

    def rows() -> list[dict[str, Any]]:
        listed = control(conn, {"op": "actions_enumerate", "query": NOTES_APP, "detail": "compact",
                                "wait_ms": 1000}, "list Notes")
        return [row for row in listed.get("actions", []) if isinstance(row, dict)]

    try:
        open_scratch_notes(state, invoke, rows, name, timeout)
    except ScratchRoomError as error:
        raise BenchError(str(error)) from error


def describe_desktop(conn: Connection, path: Path | None) -> str:
    """What the desktop shows, for a run that could not find its subject: the
    whole listing, the drawn targets and the state dump (to `path` in full)."""
    probes: dict[str, Any] = {}
    for name, payload in (
            ("listing", {"op": "actions_enumerate", "query": "", "detail": "compact",
                         "limit": 2000, "wait_ms": WAIT_MS}),
            ("windows", {"op": "windows"}),
            ("targets", {"op": "list_targets"}),
            ("state", {"op": "state_dump"})):
        try:
            _, line = conn.round_trip(request(payload))
            probes[name] = json.loads(line)
        except (OSError, BenchError, json.JSONDecodeError) as error:
            probes[name] = {"probe_error": str(error)}
    if path is not None:
        path.write_text(json.dumps(probes, indent=1, default=str), encoding="utf-8")
    answer = probes["listing"].get("answer") or {}
    listing = [r for r in answer.get("actions", []) if isinstance(r, dict)] if isinstance(answer, dict) else []
    targets = probes["targets"].get("targets") or []
    names = [t.get("name") for t in targets if isinstance(t, dict)][:60]
    return (f"whole listing: {len(listing)} rows, applications "
            f"{sorted({str(r.get('application')) for r in listing})}\n"
            f"targets ({len(targets)}): {names}" + (f"\nfull probes: {path}" if path else ""))


@contextlib.contextmanager
def seeded_desktop(shell: Path, seeder: Path, log: Path, timeout: int) -> Iterator[tuple[str, Path]]:
    """The seeded Notes desktop through spawn-headless.sh, serving its agent
    door: (control socket, runtime directory). Its profile is removed on exit."""
    if not seeder.is_file() or not os.access(seeder, os.X_OK):
        raise BenchError(f"no desktop seeder at {seeder}")
    cache = Path(os.environ.get("XDG_CACHE_HOME") or Path.home() / ".cache")
    cache.mkdir(parents=True, exist_ok=True)
    profile = Path(tempfile.mkdtemp(prefix="agent-task.", dir=cache))
    try:
        for sub in ("config/tilo/identity", "data", "state", "cache", "home", "run"):
            (profile / sub).mkdir(parents=True, exist_ok=True, mode=0o700)
        seeded = subprocess.run([str(seeder), "--profile", str(profile)], capture_output=True,
                                text=True, check=False)
        if seeded.returncode != 0:
            raise BenchError(f"the seeder failed: {seeded.stderr.strip()[-2000:]}")
        env = json.loads(seeded.stdout.strip().splitlines()[-1])
        env = {key: str(value) for key, value in env.items()}
        env["TILO_HEADLESS_PROFILE"] = str(profile)
        # spawn-headless.sh runs the compositor with XDG_RUNTIME_DIR=<profile>/run;
        # the door goes where a client in that login looks for it.
        runtime = profile / "run"
        env["TILO_AGENT_DOOR"] = str(runtime / "tilo-session" / "agent.sock")
        env.setdefault("RUST_LOG", os.environ.get("RUST_LOG", "info"))
        with spawned(shell, log, timeout, env) as socket_path:
            yield socket_path, runtime
    finally:
        shutil.rmtree(profile, ignore_errors=True)


def client_env(runtime: Path, scratch: Path | None) -> dict[str, str]:
    """The environment of an agent in a terminal of that login: its runtime
    directory, and (on a throwaway desktop) homes inside the profile."""
    env = {key: value for key, value in os.environ.items()
           if key not in ("TILO_RUNTIME_PROFILE", "TILO_RUNTIME_STANDALONE", "TILO_RUNTIME_BINARY")}
    env["XDG_RUNTIME_DIR"] = str(runtime)
    if scratch is not None:
        env.update(HOME=str(scratch / "home"), XDG_CONFIG_HOME=str(scratch / "config"),
                   XDG_DATA_HOME=str(scratch / "data"), XDG_STATE_HOME=str(scratch / "state"),
                   XDG_CACHE_HOME=str(scratch / "cache"))
    return env


def wait_for_door(path: Path, timeout: float) -> None:
    deadline = time.monotonic() + timeout
    while not path.exists():
        if time.monotonic() >= deadline:
            raise BenchError(f"the desktop never served its agent door at {path} within {timeout:.0f} s")
        time.sleep(0.1)


# --- records --------------------------------------------------------------------

def collect(runs: list[TaskRun]) -> list[tuple[str, str, list[float], dict[str, Any]]]:
    results: list[tuple[str, str, list[float], dict[str, Any]]] = []
    if not runs:
        raise BenchError("no repetition was measured")
    results.append(("agent.task.notes.wall", "ms", [r.wall_ms for r in runs], {}))
    results.append(("agent.task.notes.tilo", "ms", [r.tilo_ms for r in runs], {}))
    results.append(("agent.task.notes.model", "ms", [r.model_ms for r in runs], {}))
    results.append(("agent.task.notes.client", "ms", [r.client_ms for r in runs], {}))
    results.append(("agent.task.notes.turns", "count", [float(r.turns) for r in runs], {}))
    results.append(("agent.task.notes.retries", "count",
                    [float(sum(c.step.endswith(".retry") for c in r.calls)) for r in runs], {}))
    for step in STEPS:
        results.append((f"agent.task.notes.step.{step}", "ms", [step_ms(r, step) for r in runs],
                        {"tool": first_call(runs[0], step).name,
                         "response_bytes_max": max(first_call(r, step).response_bytes
                                                   for r in runs)}))
    return results


def step_ms(run: TaskRun, step: str) -> float:
    """What `step` cost the agent: its calls, retries and look-insides included."""
    return sum(c.ms for c in run.calls if c.step.split(".", 1)[0] == step)


def first_call(run: TaskRun, step: str) -> ToolCall:
    return next(c for c in run.calls if c.step == step)


def summary(runs: list[TaskRun]) -> str:
    def p50(values: list[float]) -> float:
        return record.nearest_rank(sorted(values), 50)
    lines = [f"agent task (Notes, agent door), {len(runs)} repetitions, p50:",
             f"  wall {p50([r.wall_ms for r in runs]):8.2f} ms   turns {p50([r.turns for r in runs]):.0f}",
             f"  tilo {p50([r.tilo_ms for r in runs]):8.2f} ms   model {p50([r.model_ms for r in runs]):.2f} ms"
             f"   client {p50([r.client_ms for r in runs]):.2f} ms"]
    for step in STEPS:
        lines.append(f"  {step:9s}{p50([step_ms(r, step) for r in runs]):8.2f} ms")
    retries = sum(c.step.endswith(".retry") for r in runs for c in r.calls)
    lines.append(f"  retried calls: {retries}")
    return "\n".join(lines)


def measure(conn: Connection, door: Door, args: argparse.Namespace) -> list[TaskRun]:
    """Warm-up and measured repetitions of the task, each with a note of its own."""
    model: FakeModel | None = None
    client: ModelClient | None = None
    tag = f"{os.getpid():x}{int(time.time()) % 100000:05d}"
    task: dict[str, NotesTask] = {}

    def respond(messages: list[dict[str, Any]]) -> list[dict[str, Any]]:
        return task["value"].respond(messages)

    try:
        model = FakeModel(respond, args.think_ms, args.tokens_per_s)
        client = ModelClient(model.port, args.tools)
        runs: list[TaskRun] = []
        for index in range(args.warmup + args.repetitions):
            task["value"] = NotesTask(title=f"Agent bench {tag} {index:03d}",
                                      body=f"Benchmark body agentbench{tag}n{index:03d} "
                                           f"written by a scripted agent.")
            try:
                run = run_task(client, door, task["value"])
            except BenchError as error:
                why = model.errors[-1] if model.errors else str(error)
                done = summary(runs) if runs else "no repetition finished"
                raise BenchError(f"repetition {index}: {why}\nbefore it: {done}\n"
                                 f"{what_the_agent_sees(door, task['value'].title)}") from error
            if index >= args.warmup:
                runs.append(run)
        return runs
    finally:
        if client is not None:
            client.close()
        if model is not None:
            model.close()


def what_the_agent_sees(door: Door, title: str) -> str:
    """The door's own view of Notes, for a run that could not find its note."""
    lines = []
    for name, arguments in (("tilo_ls", {"path": "notes/Notebook"}),
                            ("tilo_ls", {"path": f"notes/Notebook/{title}"}),
                            ("tilo_read", {"path": f"notes/Notebook/{title}"}),
                            ("tilo_find", {"query": title, "app": "notes"}),
                            ("tilo_find", {"query": title})):
        try:
            result, _, _ = door.call(name, arguments)
            lines.append(f"{name} {json.dumps(arguments)}{' (error)' if result.failed else ''}:\n"
                         f"{describe(result.text, 3000)}")
        except BenchError as error:
            lines.append(f"{name} {json.dumps(arguments)}: {error}")
    return "\n".join(lines)


def session(socket_path: str, runtime: Path, args: argparse.Namespace,
            scratch: Path | None) -> list[TaskRun]:
    """Admit the agent on this desktop, run the task, and take the agent back
    out when the desktop is a person's."""
    conn = Connection(socket_path, timeout=60)
    door: Door | None = None
    try:
        if args.own_notebook:
            stamp = time.strftime("%Y-%m-%d %H:%M:%S")
            open_own_notebook(conn, f"Agent task bench {stamp}", args.startup_timeout)
        wait_for_door(runtime / "tilo-session" / "agent.sock", args.startup_timeout)
        door = Door.start(args.mcp, client_env(runtime, scratch), args.client_log)
        args.tools = admit(conn, door, args.startup_timeout)
        log = args.log
        wait_for_notes(door, conn, args.startup_timeout,
                       log.with_name(log.stem + ".probes.json") if log else None)
        return measure(conn, door, args)
    finally:
        if door is not None:
            door.close()
        if args.own_notebook:
            # The agent leaves the person's desktop as it found it.
            with contextlib.suppress(OSError, BenchError):
                control(conn, {"op": "actions_invoke", "provider": "actions", "id": "agent.remove",
                               "args": {"name": AGENT, "confirm": True}, "wait_ms": WAIT_MS},
                        "remove the agent")
        conn.close()


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    target = parser.add_mutually_exclusive_group(required=True)
    target.add_argument("--socket", type=Path,
                        help="a running desktop's admin control socket; its agent.sock is beside "
                        "it, and the run opens Notes in a new workspace of its own")
    target.add_argument("--spawn", type=Path, metavar="SHELL",
                        help="start this compositor headless on a seeded Notes desktop")
    parser.add_argument("--seeder", type=Path,
                        help="the agent_task_desktop example (default beside SHELL)")
    parser.add_argument("--mcp", type=Path,
                        help="tilo-runtime-mcp (default beside SHELL, else on PATH)")
    parser.add_argument("--repetitions", type=int, default=30)
    parser.add_argument("--warmup", type=int, default=3)
    parser.add_argument("--think-ms", type=float, default=0.0,
                        help="the fake model's time to its first content block, per turn")
    parser.add_argument("--tokens-per-s", type=float, default=0.0,
                        help="the fake model's streaming rate; 0 streams at once")
    parser.add_argument("--transport", default=None,
                        help="how the desktop is reached, recorded in each record's meta")
    parser.add_argument("--env-json", type=Path,
                        help="record.py env output; without it, summaries are printed instead")
    parser.add_argument("--out", type=Path, help="append records here (default stdout)")
    parser.add_argument("--log", type=Path, help="the spawned compositor's output")
    parser.add_argument("--client-log", type=Path, help="tilo-runtime-mcp's stderr")
    parser.add_argument("--startup-timeout", type=int, default=120)
    args = parser.parse_args(argv)
    if args.repetitions < 1 or args.warmup < 0:
        parser.error("--repetitions must be positive and --warmup not negative")
    if args.transport is None:
        args.transport = ("tilo-runtime-mcp --connect, agent door, headless compositor" if args.spawn
                          else "tilo-runtime-mcp --connect, agent door")
    # Only the seeded throwaway desktop's notebook is the bench's; on any
    # running desktop the notebooks belong to a person.
    args.own_notebook = not args.spawn
    if args.mcp is None:
        beside = args.spawn.resolve().parent / "tilo-runtime-mcp" if args.spawn else None
        found = shutil.which("tilo-runtime-mcp")
        args.mcp = beside if beside and beside.is_file() else Path(found) if found else Path("tilo-runtime-mcp")
    env = record.load_env(args.env_json) if args.env_json else None
    try:
        if args.spawn:
            seeder = args.seeder or args.spawn.resolve().parent / "examples" / "agent_task_desktop"
            log = args.log or Path(tempfile.gettempdir()) / "agent-task-compositor.log"
            args.log = log
            with seeded_desktop(args.spawn, seeder, log, args.startup_timeout) as (socket_path, runtime):
                runs = session(socket_path, runtime, args, runtime.parent)
        else:
            runs = session(str(args.socket), args.socket.resolve().parent.parent, args, None)
        results = collect(runs)
    except (BenchError, OSError) as error:
        print(f"agent_task: {error}", file=sys.stderr)
        return 1
    print(summary(runs), file=sys.stderr)
    common = {"transport": args.transport, "task": "notes: find, create, locate, write, read",
              "model": MODEL_NAME, "think_ms": args.think_ms, "tokens_per_s": args.tokens_per_s,
              "warmup": args.warmup}
    emit(results, common, env, args.out)
    return 0


def emit(results: list[tuple[str, str, list[float], dict[str, Any]]], common: dict[str, Any],
         env: dict[str, Any] | None, out: Path | None) -> None:
    for kpi, unit, samples, meta in results:
        meta = {**common, **meta}
        if env is None:
            ordered = sorted(samples)
            print(json.dumps({"kpi": kpi, "unit": unit, "n": len(ordered),
                              "p50": record.nearest_rank(ordered, 50),
                              "p95": record.nearest_rank(ordered, 95),
                              "max": ordered[-1], "meta": meta}))
            continue
        item = record.from_samples(kpi, unit, samples, **{**env, "meta": {**env.get("meta", {}), **meta}})
        if out is None:
            print(item.to_json())
        else:
            record.write(out, item)


if __name__ == "__main__":
    sys.exit(main())
