#!/usr/bin/env python3
"""An agent task, end to end: a scripted model drives Notes through the action surface.

The task is the one a person gives an agent: "Create a note titled T with
the body B in Notes, then find it by searching for its text." It runs the
way an external agent (Claude Code, any MCP client) runs it, with the model
replaced so the number measures Tilo and needs no key:

  agent loop  --HTTP, Anthropic Messages streaming-->  fake model (loopback)
  agent loop  --control socket JSON lines---------->  compositor -> Notes

The fake model is a loopback HTTP server that answers each turn with a
streamed `tool_use` (server-sent events, as the Messages API streams them).
Its policy reads the previous tool results the way a perfect model would and
picks the next call from what Tilo published; it never knows an address in
advance. `--think-ms` delays each turn's first content block and
`--tokens-per-s` paces the stream, so a run can model a real provider; the
scheduled lane runs them at 0, and the model's share is then only the fake
server's loopback overhead, reported apart.

The agent loop posts each turn, parses the stream and runs the tool call
over one persistent control-socket connection, the transport `tilo-control`
and the MCP shim use:

  tilo_enumerate_actions  actions_enumerate {query, detail: compact,
                          wait_ms} and every next page; `wait_ms` holds the
                          answer until no app directory page is still owed,
                          so a search sees what the apps have published
  tilo_invoke_action      actions_invoke {provider, id, args, wait_ms}: one
                          round trip that answers with the terminal receipt

The steps, one tool call per turn because each needs the last one's result:

  discover  search the actions for "New note"
  create    invoke Notes' notebook `new-page` with the title
  locate    search the actions for the title: the new note's body
  write     invoke the body's `append-paragraph` with the body text
  search    search for a phrase of the body: the note's block hit
  open      invoke the hit: Notes opens the note at that block

then a final text turn. Each step is timed from the request written to the
whole answer read. With `--split-repetitions N`, N more repetitions invoke
without `wait_ms` (the admission receipt: "accept") and then wait on
`actions_status` ("apply"), the invoke -> app applied -> result returned
breakdown; those runs pay one more round trip per invoke, so they are not
the task's KPI.

Records (tools/perf/record.py), one sample per repetition:

  agent.task.notes.wall          the task end to end, ms
  agent.task.notes.tilo          the six tool calls together, ms
  agent.task.notes.model         the seven model turns together (fake), ms
  agent.task.notes.client        the agent loop's own work, ms
  agent.task.notes.turns         model turns the task took
  agent.task.notes.step.<step>   one step's tool call, ms
  agent.task.notes.step.<create|write|open>.accept / .apply  (split runs)

The desktop is `crates/session/shell/examples/agent_task_desktop.rs`: a
signed-in person with one empty Notes notebook, restored from a seeded
profile. Authorization and the capability gate stay on; Notes is the
person's own notebook, so no step asks for consent. The compositor, its
profile and its socket are removed on exit, whatever happens. Against a
running desktop (`--socket`, e.g. a VM's admin-control.sock) nothing is
seeded, and the task never touches the notebook a person already has: every
Notes window in a workspace opens that workspace's notebook, so the run first
creates a workspace of its own (a new room), opens Notes there and only acts
on Notes rows of that room and record (`scratch_notes.py`).
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
    TERMINAL, BenchError, Connection, answer_of, receipt_state, request, spawned)
from scratch_notes import NOTES_APP, ScratchNotes, ScratchRoomError, open_scratch_notes  # noqa: E402
# Generous: each answer leaves as soon as its invocation or listing settles.
WAIT_MS = 5000
# How long a search may wait for the apps' directory pages to complete it
# (`--listing-wait-ms`). An answer still incomplete then is what the agent
# works from, as one through the MCP door (which does not wait) would be.
LISTING_WAIT_MS = 1000
STEPS = ("discover", "create", "locate", "write", "search", "open")
INVOKE_STEPS = ("create", "write", "open")
# The fake model's Messages API version and model name, echoed in records.
MODEL_NAME = "tilo-scripted-notes-task"
# Rough characters per token, to pace a streamed answer at --tokens-per-s.
CHARS_PER_TOKEN = 4

TOOLS = [
    {
        "name": "tilo_enumerate_actions",
        "description": "List the typed actions the running Tilo desktop exports for a query, "
                       "through the same registry the command palette searches. Read-only.",
        "input_schema": {"type": "object", "properties": {"query": {"type": "string"}},
                         "required": []},
    },
    {
        "name": "tilo_invoke_action",
        "description": "Invoke one enumerated action by its (provider, id) address with typed "
                       "arguments; answers once it has settled.",
        "input_schema": {
            "type": "object",
            "properties": {"provider": {"type": "string"}, "id": {"type": "string"},
                           "args": {"type": "object", "additionalProperties": True}},
            "required": ["provider", "id"],
        },
    },
]


# --- the fake model -------------------------------------------------------------

class PolicyError(RuntimeError):
    """The scripted model found nothing to do with what Tilo answered."""


def conversation_calls(messages: list[dict[str, Any]]) -> list[tuple[dict[str, Any], Any]]:
    """Every (tool_use block, parsed tool_result) pair of the conversation, in order."""
    uses: dict[str, dict[str, Any]] = {}
    pairs: list[tuple[dict[str, Any], Any]] = []
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
                try:
                    result = json.loads(text) if isinstance(text, str) else text
                except json.JSONDecodeError:
                    result = text
                pairs.append((use, result))
    return pairs


def rows_of(result: Any) -> list[dict[str, Any]]:
    if isinstance(result, dict) and isinstance(result.get("actions"), list):
        return [row for row in result["actions"] if isinstance(row, dict)]
    raise PolicyError(f"expected a listing, got {json.dumps(result)[:300]}")


def notes_row(row: dict[str, Any]) -> bool:
    return row.get("application") == NOTES_APP or NOTES_APP in str(row.get("id", ""))


def operation_of(row: dict[str, Any]) -> str:
    operation = row.get("operation")
    if isinstance(operation, str) and operation:
        return operation
    # A provider without hierarchy names the operation last in its id.
    return re.split(r"[\x1f/#:]", str(row.get("id", "")))[-1]


def text_param(row: dict[str, Any]) -> str:
    """The name of the row's one required text parameter."""
    params = row.get("params") or []
    names = [p.get("name") for p in params
             if isinstance(p, dict) and p.get("required", True) and p.get("name")]
    if len(names) != 1:
        raise PolicyError(f"expected one required parameter on {row.get('id')!r}: {params}")
    return str(names[0])


def describe_rows(rows: list[dict[str, Any]], limit: int = 40) -> str:
    return "; ".join(
        f"{row.get('title')!r} op={operation_of(row)} app={row.get('application')} "
        f"res={row.get('resource')}" for row in rows[:limit])


def invoke_input(row: dict[str, Any], args: dict[str, Any]) -> dict[str, Any]:
    return {"provider": row["provider"], "id": row["id"], "args": args}


@dataclass
class NotesTask:
    """The scripted model of the Notes task: one tool call per turn, then a reply.

    A step's listing discloses the address its invoke uses. When the desktop
    refuses that address as gone ("list it again"), the model lists again and
    retries, as a model reading the refusal would, at most MAX_RETRIES times
    a task; those calls are named `<step>.retry` and count to their step.
    """

    title: str
    body: str
    phrase: str
    # The step of every tool call this model issued, in order.
    steps: list[str] = field(default_factory=list)
    retries: int = 0
    # The run's own notebook against a running desktop; Notes rows of any
    # other notebook are never chosen. None on a seeded throwaway desktop.
    notes: ScratchNotes | None = None

    def visible(self, rows: list[dict[str, Any]]) -> list[dict[str, Any]]:
        """What the model may act on: every row but another notebook's Notes rows."""
        return [r for r in rows if self.notes is None or not notes_row(r) or self.notes.owns(r)]

    def prompt(self) -> str:
        return (f"Create a note titled {self.title!r} with the body {self.body!r} in Notes, "
                f"then find it by searching for {self.phrase!r} and open it.")

    # Each invoke step and the listing step that discloses its address.
    LISTING = {"create": "discover", "write": "locate", "open": "search"}
    MAX_RETRIES = 8

    def respond(self, messages: list[dict[str, Any]]) -> list[dict[str, Any]]:
        """The next turn's content blocks for this conversation."""
        done = conversation_calls(messages)
        if not done:
            return [self.call("discover")]
        last = done[-1][1]
        step = self.steps[len(done) - 1].removesuffix(".retry")
        if isinstance(last, dict) and "error" in last:
            if step not in self.LISTING or not retriable(last) or self.retries >= self.MAX_RETRIES:
                raise PolicyError(f"{step} was refused: {json.dumps(last)[:400]}")
            self.retries += 1
            return [self.call(self.LISTING[step], retry=True)]
        if step == "discover":
            rows = self.visible(rows_of(last))
            found = [r for r in rows if notes_row(r) and operation_of(r) == "new-page"]
            if not found:
                raise PolicyError(f"no Notes new-page among {len(rows)} rows: {describe_rows(rows)}")
            return [self.call("create", found[0], {text_param(found[0]): self.title})]
        if step == "create":
            check_succeeded(last, "create")
            return [self.call("locate")]
        if step == "locate":
            row = self.body_action(self.visible(rows_of(last)))
            return [self.call("write", row, {text_param(row): self.body})]
        if step == "write":
            check_succeeded(last, "write")
            return [self.call("search")]
        if step == "search":
            rows = self.visible(rows_of(last))
            # The phrase is in one block of one note, so the hit is that
            # block's `open` ("Reveal block"), or a row naming the phrase.
            hits = [r for r in rows if self.phrase in str(r.get("title", ""))
                    or (notes_row(r) and operation_of(r) == "open"
                        and resource_of(r).startswith("notes.block:"))]
            hits.sort(key=lambda r: not notes_row(r))
            if not hits:
                raise PolicyError(f"no Notes hit for {self.phrase!r} among {len(rows)} rows: "
                                  f"{describe_rows(rows)}")
            return [self.call("open", hits[0], {})]
        if step == "open":
            check_succeeded(last, "open")
            return [{"type": "text", "text": f"Created {self.title!r} and opened it from search."}]
        raise PolicyError(f"the task has no step after {step}")

    def call(self, step: str, row: dict[str, Any] | None = None,
             args: dict[str, Any] | None = None, retry: bool = False) -> dict[str, Any]:
        """The tool call of `step`; an invoke step's after a retried listing
        is a retry too."""
        if retry or (row is not None and self.steps and self.steps[-1].endswith(".retry")):
            name = f"{step}.retry"
        else:
            name = step
        self.steps.append(name)
        if row is None:
            query = {"discover": "New note", "locate": self.title, "search": self.phrase}[step]
            return tool("tilo_enumerate_actions", {"query": query})
        return tool("tilo_invoke_action", invoke_input(row, args or {}))

    def body_action(self, rows: list[dict[str, Any]]) -> dict[str, Any]:
        """The new note's body `append-paragraph`. The listing for the title
        holds the note (`notes.page:<id>`) and its body, a child resource
        (`…/page/<id>/body`) labelled with the title; read the ids the way a
        model reads the listing it was given."""
        notes = [r for r in rows if notes_row(r)]
        appends = [r for r in notes if operation_of(r) == "append-paragraph"]
        pages = {m.group(1) for r in notes
                 for m in [re.search(r"notes\.page:([0-9a-f-]{36})", resource_of(r))] if m}
        bodies = [r for r in appends
                  if (m := re.search(r"page/([0-9a-f-]{36})/body", resource_of(r)))
                  and m.group(1) in pages]
        if len(bodies) == 1:
            return bodies[0]
        if not pages and len(appends) == 1:
            return appends[0]
        raise PolicyError(f"no single append-paragraph on {self.title!r} ({len(pages)} notes, "
                          f"{len(appends)} bodies) among {len(rows)} rows: {describe_rows(rows)}")


def resource_of(row: dict[str, Any]) -> str:
    """A row's resource id, with a bound resource's hex-encoded tail read
    (`tilo-ui-bound:v1:<room>:<record>:<hex>`)."""
    resource = str(row.get("resource") or "")
    tail = resource.rsplit(":", 1)[-1]
    if resource.startswith("tilo-ui-bound:") and len(tail) % 2 == 0:
        try:
            return bytes.fromhex(tail).decode()
        except ValueError:
            pass
    return resource


def retriable(result: dict[str, Any]) -> bool:
    """Whether a refusal says the address went stale: list it again."""
    error = result.get("error") or {}
    message = str(error.get("message", ""))
    return error.get("code") == "unknown_action" or "list it again" in message


def check_succeeded(result: Any, what: str) -> None:
    state = receipt_state(result)
    if state != "succeeded":
        raise PolicyError(f"{what} ended {state!r}: {json.dumps(result)[:300]}")


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
        # One delta per ~token, as providers stream: 4 characters each.
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
    accept_ms: float | None = None
    apply_ms: float | None = None


@dataclass
class TaskRun:
    turns: int = 0
    wall_ms: float = 0.0
    model_ms: float = 0.0
    model_first_byte_ms: list[float] = field(default_factory=list)
    calls: list[ToolCall] = field(default_factory=list)
    # Listings that answered unsettled at their wait_ms (Tools.unsettled).
    unsettled: list[dict[str, Any]] = field(default_factory=list)

    @property
    def tilo_ms(self) -> float:
        return sum(call.ms for call in self.calls)

    @property
    def client_ms(self) -> float:
        return self.wall_ms - self.model_ms - self.tilo_ms


class ModelClient:
    """Streams one Messages turn over a kept-alive loopback connection."""

    def __init__(self, port: int):
        self.connection = http.client.HTTPConnection("127.0.0.1", port, timeout=120)

    def close(self) -> None:
        self.connection.close()

    def turn(self, messages: list[dict[str, Any]]) -> tuple[list[dict[str, Any]], str, float]:
        """(content blocks, stop reason, ms to the first streamed byte)."""
        body = json.dumps({"model": MODEL_NAME, "max_tokens": 1024, "stream": True,
                           "tools": TOOLS, "messages": messages})
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


class Refused(BenchError):
    """The desktop refused an invocation; the model reads why."""

    def __init__(self, code: str, message: str):
        super().__init__(f"{code}: {message}")
        self.code = code
        self.message = message


def answer_or_refusal(line: bytes, what: str) -> Any:
    """`answer_of`, with a refusal raised as Refused rather than a bench failure."""
    reply = json.loads(line)
    if isinstance(reply, dict) and "err" in reply:
        err = reply["err"] or {}
        raise Refused(str(err.get("code")), str(err.get("message")))
    return answer_of(line, what)


class Tools:
    """The agent's tools, run on one persistent control-socket connection."""

    def __init__(self, conn: Connection, split: bool, unsettled: list[dict[str, Any]] | None = None,
                 listing_wait_ms: int = LISTING_WAIT_MS):
        self.conn = conn
        self.split = split
        self.listing_wait_ms = listing_wait_ms
        # Listings that answered at their wait_ms without settling: what an
        # agent then works from is what the desktop had, and the step paid
        # the whole wait. Reported, never hidden.
        self.unsettled = unsettled if unsettled is not None else []

    def run(self, name: str, arguments: dict[str, Any], step: str) -> tuple[Any, ToolCall]:
        if name == "tilo_enumerate_actions":
            return self.enumerate(str(arguments.get("query", "")), step)
        if name == "tilo_invoke_action":
            started = time.perf_counter_ns()
            try:
                return self.invoke(arguments, step)
            except Refused as refusal:
                # The refusal is the tool's result, as an MCP tool error is:
                # the model reads it and decides.
                return ({"error": {"code": refusal.code, "message": refusal.message}},
                        ToolCall(step, name, (time.perf_counter_ns() - started) / 1e6,
                                 len(refusal.message)))
        raise BenchError(f"the model called an unknown tool {name!r}")

    def enumerate(self, query: str, step: str) -> tuple[Any, ToolCall]:
        payload: dict[str, Any] = {"op": "actions_enumerate", "query": query,
                                   "detail": "compact", "wait_ms": self.listing_wait_ms}
        rows: list[Any] = []
        total_ns = 0
        size = 0
        settled = None
        while True:
            elapsed, line = self.conn.round_trip(request(payload))
            total_ns += elapsed
            size += len(line)
            answer = answer_of(line, f"{step}: actions_enumerate {query!r}")
            if settled is None:
                settled = answer.get("settled", answer.get("complete"))
                first = {key: value for key, value in answer.items() if key != "actions"}
            rows.extend(answer.get("actions", []))
            cursor = answer.get("next_cursor")
            if not cursor:
                break
            payload = {"op": "actions_enumerate", "query": query, "detail": "compact",
                       "cursor": cursor}
        if settled is False:
            self.unsettled.append({
                "step": step, "query": query, "ms": round(total_ns / 1e6, 1), "rows": len(rows),
                "providers": sorted({str(r.get("provider")) for r in rows if isinstance(r, dict)}),
                "answer": first})
        result = {"query": query, "count": len(rows), "actions": rows}
        return result, ToolCall(step, "tilo_enumerate_actions", total_ns / 1e6, size)

    def invoke(self, arguments: dict[str, Any], step: str) -> tuple[Any, ToolCall]:
        payload: dict[str, Any] = {"op": "actions_invoke", "provider": arguments.get("provider"),
                                   "id": arguments.get("id"), "args": arguments.get("args") or {}}
        if not self.split:
            payload["wait_ms"] = WAIT_MS
            elapsed, line = self.conn.round_trip(request(payload))
            answer = answer_or_refusal(line, f"{step}: actions_invoke")
            settled(answer, step)
            return answer, ToolCall(step, "tilo_invoke_action", elapsed / 1e6, len(line))
        accept_ns, line = self.conn.round_trip(request(payload))
        answer = answer_or_refusal(line, f"{step}: actions_invoke")
        apply_ns = 0
        size = len(line)
        if receipt_state(answer) not in TERMINAL:
            job = (answer.get("receipt") or {}).get("broker_invocation_id")
            if not isinstance(job, int):
                raise BenchError(f"{step}: an unsettled receipt names no invocation: "
                                 f"{json.dumps(answer)[:400]}")
            apply_ns, line = self.conn.round_trip(request(
                {"op": "actions_status", "invocation_id": job, "wait_ms": WAIT_MS}))
            size += len(line)
            status = answer_of(line, f"{step}: actions_status")
            answer = {**answer, "receipt": status.get("receipt", status)}
        settled(answer, step)
        return answer, ToolCall(step, "tilo_invoke_action", (accept_ns + apply_ns) / 1e6, size,
                                accept_ns / 1e6, apply_ns / 1e6)


def settled(answer: Any, step: str) -> None:
    state = receipt_state(answer)
    if state not in TERMINAL:
        raise BenchError(f"{step}: wait_ms answered before the invocation settled (state {state!r})")
    if state != "succeeded":
        raise BenchError(f"{step}: the invocation ended {state}: {json.dumps(answer)[:600]}")


def run_task(model: ModelClient, tools: Tools, prompt: str, max_turns: int = 24,
             steps: list[str] | None = None) -> TaskRun:
    """One task: model turns and tool calls until the model ends its turn."""
    run = TaskRun()
    messages: list[dict[str, Any]] = [{"role": "user", "content": prompt}]
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
            index = len(run.calls)
            if steps is not None and index < len(steps):
                step = steps[index]
            else:
                step = STEPS[index] if index < len(STEPS) else f"extra{index}"
            result, call = tools.run(block["name"], block["input"], step)
            run.calls.append(call)
            results.append({"type": "tool_result", "tool_use_id": block["id"],
                            "content": json.dumps(result, separators=(",", ":"))})
        messages.append({"role": "user", "content": results})
    run.wall_ms = (time.perf_counter_ns() - start) / 1e6
    run.unsettled = list(tools.unsettled)
    if [call.step for call in run.calls if not call.step.endswith(".retry")] != list(STEPS):
        raise BenchError(f"the task ran {[c.step for c in run.calls]}, not {list(STEPS)}")
    return run


# --- the desktop ----------------------------------------------------------------

def wait_for_notes(conn: Connection, timeout: float, diagnostics: Path | None = None) -> None:
    """Until the restored notebook publishes `new-page`; fail with what the desktop showed."""
    deadline = time.monotonic() + timeout
    rows: list[Any] = []
    while time.monotonic() < deadline:
        _, line = conn.round_trip(request({"op": "actions_enumerate", "query": "New note",
                                           "detail": "compact", "wait_ms": WAIT_MS}))
        rows = answer_of(line, "actions_enumerate").get("actions", [])
        if any(notes_row(r) and operation_of(r) == "new-page" for r in rows):
            return
        time.sleep(0.2)
    seen = describe_desktop(conn, diagnostics)
    raise BenchError(f"Notes never published new-page within {timeout:.0f} s; "
                     f"'New note' listed {len(rows)}: {describe_rows(rows)}\n{seen}")


def open_own_notebook(conn: Connection, name: str, timeout: float) -> ScratchNotes:
    """A workspace of the run's own with Notes open in it, over the control socket."""
    def state() -> dict[str, Any]:
        _, line = conn.round_trip(request({"op": "state_dump"}))
        reply = answer_of(line, "state_dump")
        return reply.get("state", reply) if isinstance(reply, dict) else {}

    def invoke(provider: str, action: str, args: dict[str, Any]) -> Any:
        _, line = conn.round_trip(request({"op": "actions_invoke", "provider": provider,
                                           "id": action, "args": args, "wait_ms": WAIT_MS}))
        answer = answer_of(line, f"{provider} {action}")
        settled(answer, f"{provider} {action}")
        return answer

    def rows() -> list[dict[str, Any]]:
        listing, _ = Tools(conn, split=False).enumerate(NOTES_APP, "own-notebook")
        return listing["actions"]

    try:
        return open_scratch_notes(state, invoke, rows, name, timeout)
    except ScratchRoomError as error:
        raise BenchError(str(error)) from error


def describe_desktop(conn: Connection, path: Path | None) -> str:
    """What the desktop shows, for a run that could not find its subject: the
    whole listing, the drawn targets and the state dump (to `path` in full)."""
    probes: dict[str, Any] = {}
    for name, payload in (
            ("listing", {"op": "actions_enumerate", "query": "", "detail": "compact",
                         "limit": 2000, "wait_ms": WAIT_MS}),
            ("listing_note", {"op": "actions_enumerate", "query": "note", "detail": "compact",
                              "wait_ms": WAIT_MS}),
            ("listing_app", {"op": "actions_enumerate", "query": NOTES_APP, "detail": "compact",
                             "include_programmatic": True, "wait_ms": WAIT_MS}),
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

    def rows(name: str) -> list[dict[str, Any]]:
        answer = probes[name].get("answer") or {}
        return [r for r in answer.get("actions", []) if isinstance(r, dict)] if isinstance(answer, dict) else []

    listing = rows("listing")
    applications = sorted({str(r.get("application")) for r in listing})
    targets = probes["targets"].get("targets") or []
    names = [t.get("name") for t in targets if isinstance(t, dict)][:60]
    state = probes["state"]
    capture = json.dumps(state)[:0]
    with contextlib.suppress(AttributeError, TypeError):
        capture = json.dumps(state.get("chrome", state.get("state", {}).get("chrome", {}))
                             .get("input_capture"))
    return (f"whole listing: {len(listing)} rows, applications {applications}; "
            f"providers {sorted({str(r.get('provider')) for r in listing})}\n"
            f"'note' listed: {describe_rows(rows('listing_note'))}\n"
            f"'{NOTES_APP}' listed: {describe_rows(rows('listing_app'))}\n"
            f"targets ({len(targets)}): {names}\ninput capture: {capture}\n"
            f"state keys: {sorted(state) if isinstance(state, dict) else type(state)}"
            + (f"\nfull probes: {path}" if path else ""))


@contextlib.contextmanager
def seeded_desktop(shell: Path, seeder: Path, log: Path, timeout: int) -> Iterator[str]:
    """The seeded Notes desktop through spawn-headless.sh; its profile removed on exit."""
    if not seeder.is_file() or not os.access(seeder, os.X_OK):
        raise BenchError(f"no desktop seeder at {seeder}")
    cache = Path(os.environ.get("XDG_CACHE_HOME") or Path.home() / ".cache")
    cache.mkdir(parents=True, exist_ok=True)
    profile = Path(tempfile.mkdtemp(prefix="agent-task.", dir=cache))
    try:
        for sub in ("config/tilo/identity", "data", "state", "cache", "home"):
            (profile / sub).mkdir(parents=True, exist_ok=True, mode=0o700)
        seeded = subprocess.run([str(seeder), "--profile", str(profile)], capture_output=True,
                                text=True, check=False)
        if seeded.returncode != 0:
            raise BenchError(f"the seeder failed: {seeded.stderr.strip()[-2000:]}")
        env = json.loads(seeded.stdout.strip().splitlines()[-1])
        env = {key: str(value) for key, value in env.items()}
        env["TILO_HEADLESS_PROFILE"] = str(profile)
        # What the session says while it starts is the first thing to read
        # when a run cannot find Notes.
        env.setdefault("RUST_LOG", os.environ.get("RUST_LOG", "info,tilo::listing=debug"))
        with spawned(shell, log, timeout, env) as socket_path:
            yield socket_path
    finally:
        shutil.rmtree(profile, ignore_errors=True)


# --- records --------------------------------------------------------------------

def collect(runs: list[TaskRun], split_runs: list[TaskRun]) -> list[tuple[str, str, list[float], dict[str, Any]]]:
    results: list[tuple[str, str, list[float], dict[str, Any]]] = []
    if not runs:
        raise BenchError("no repetition was measured")
    unsettled = sum(len(r.unsettled) for r in runs)
    results.append(("agent.task.notes.wall", "ms", [r.wall_ms for r in runs],
                    {"unsettled_listings": unsettled}))
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
    for step in STEPS:
        if step not in INVOKE_STEPS or not split_runs:
            continue
        calls = [first_call(r, step) for r in split_runs]
        results.append((f"agent.task.notes.step.{step}.accept", "ms",
                        [c.accept_ms or 0.0 for c in calls], {"split": True}))
        results.append((f"agent.task.notes.step.{step}.apply", "ms",
                        [c.apply_ms or 0.0 for c in calls], {"split": True}))
    return results


def step_ms(run: TaskRun, step: str) -> float:
    """What `step` cost the agent: its call, and its retries'."""
    return sum(c.ms for c in run.calls if c.step.removesuffix(".retry") == step)


def first_call(run: TaskRun, step: str) -> ToolCall:
    return next(c for c in run.calls if c.step == step)


def summary(runs: list[TaskRun]) -> str:
    def p50(values: list[float]) -> float:
        return record.nearest_rank(sorted(values), 50)
    lines = [f"agent task (Notes), {len(runs)} repetitions, p50:",
             f"  wall {p50([r.wall_ms for r in runs]):8.2f} ms   turns {p50([r.turns for r in runs]):.0f}",
             f"  tilo {p50([r.tilo_ms for r in runs]):8.2f} ms   model {p50([r.model_ms for r in runs]):.2f} ms"
             f"   client {p50([r.client_ms for r in runs]):.2f} ms"]
    for step in STEPS:
        lines.append(f"  {step:9s}{p50([step_ms(r, step) for r in runs]):8.2f} ms")
    retries = sum(c.step.endswith(".retry") for r in runs for c in r.calls)
    lines.append(f"  retried calls: {retries}")
    return "\n".join(lines)


def measure(socket_path: str, args: argparse.Namespace) -> tuple[list[TaskRun], list[TaskRun]]:
    conn = Connection(socket_path, timeout=60)
    model: FakeModel | None = None
    client: ModelClient | None = None
    tag = f"{os.getpid():x}{int(time.time()) % 100000:05d}"
    task = {"value": None}

    def respond(messages: list[dict[str, Any]]) -> list[dict[str, Any]]:
        assert task["value"] is not None
        return task["value"].respond(messages)

    try:
        log = getattr(args, "log", None)
        notes = None
        if args.own_notebook:
            notes = open_own_notebook(conn, f"Agent bench {tag}", args.startup_timeout)
        else:
            wait_for_notes(conn, args.startup_timeout,
                           log.with_name(log.stem + ".probes.json") if log else None)
        model = FakeModel(respond, args.think_ms, args.tokens_per_s)
        client = ModelClient(model.port)
        runs: list[TaskRun] = []
        split_runs: list[TaskRun] = []
        plan = ([("warmup", False)] * args.warmup + [("measure", False)] * args.repetitions
                + [("split", True)] * args.split_repetitions)
        for index, (kind, split) in enumerate(plan):
            phrase = f"agentbench{tag}n{index:03d}"
            task["value"] = NotesTask(title=f"Agent bench {tag} {index:03d}",
                                      body=f"Benchmark body {phrase} written by a scripted agent.",
                                      phrase=phrase, notes=notes)
            tools = Tools(conn, split, listing_wait_ms=getattr(args, "listing_wait_ms", LISTING_WAIT_MS))
            try:
                run = run_task(client, tools, task["value"].prompt(), steps=task["value"].steps)
            except BenchError as error:
                why = model.errors[-1] if model.errors else str(error)
                seen = "".join(f"\n  unsettled: {json.dumps(entry)[:1500]}" for entry in tools.unsettled)
                raise BenchError(f"repetition {index}: {why}{seen}") from error
            if kind == "measure":
                runs.append(run)
            elif kind == "split":
                split_runs.append(run)
        return runs, split_runs
    finally:
        if client is not None:
            client.close()
        if model is not None:
            model.close()
        conn.close()


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    target = parser.add_mutually_exclusive_group(required=True)
    target.add_argument("--socket", help="a running desktop's control socket; the run opens "
                        "Notes in a new workspace of its own")
    target.add_argument("--spawn", type=Path, metavar="SHELL",
                        help="start this compositor headless on a seeded Notes desktop")
    parser.add_argument("--seeder", type=Path,
                        help="the agent_task_desktop example (default beside SHELL)")
    parser.add_argument("--repetitions", type=int, default=30)
    parser.add_argument("--warmup", type=int, default=3)
    parser.add_argument("--split-repetitions", type=int, default=10,
                        help="repetitions that time accept and apply apart (not the KPI)")
    parser.add_argument("--listing-wait-ms", type=int, default=LISTING_WAIT_MS,
                        help="how long a search waits for the apps to complete it (default %(default)s)")
    parser.add_argument("--think-ms", type=float, default=0.0,
                        help="the fake model's time to its first content block, per turn")
    parser.add_argument("--tokens-per-s", type=float, default=0.0,
                        help="the fake model's streaming rate; 0 streams at once")
    parser.add_argument("--transport", default=None,
                        help="how the socket is reached, recorded in each record's meta")
    parser.add_argument("--env-json", type=Path,
                        help="record.py env output; without it, summaries are printed instead")
    parser.add_argument("--out", type=Path, help="append records here (default stdout)")
    parser.add_argument("--log", type=Path, help="the spawned compositor's stderr")
    parser.add_argument("--startup-timeout", type=int, default=120)
    args = parser.parse_args(argv)
    if args.repetitions < 1 or args.warmup < 0 or args.split_repetitions < 0:
        parser.error("--repetitions must be positive, --warmup and --split-repetitions not negative")
    if args.transport is None:
        args.transport = ("test-control socket, headless compositor" if args.spawn
                          else "control socket")
    # Only the seeded throwaway desktop's notebook is the bench's; on any
    # running desktop the notebooks belong to a person.
    args.own_notebook = not args.spawn
    env = record.load_env(args.env_json) if args.env_json else None
    try:
        if args.spawn:
            seeder = args.seeder or args.spawn.resolve().parent / "examples" / "agent_task_desktop"
            log = args.log or Path(tempfile.gettempdir()) / "agent-task-compositor.log"
            with seeded_desktop(args.spawn, seeder, log, args.startup_timeout) as socket_path:
                runs, split_runs = measure(socket_path, args)
        else:
            runs, split_runs = measure(args.socket, args)
        results = collect(runs, split_runs)
    except (BenchError, OSError) as error:
        print(f"agent_task: {error}", file=sys.stderr)
        return 1
    print(summary(runs), file=sys.stderr)
    unsettled = [entry for run in runs + split_runs for entry in run.unsettled]
    if unsettled:
        print(f"agent_task: {len(unsettled)} listings answered unsettled at "
              f"{args.listing_wait_ms} ms; first:",
              file=sys.stderr)
        for entry in unsettled[:5]:
            print(f"  {json.dumps(entry)[:1500]}", file=sys.stderr)
    common = {"transport": args.transport, "task": "notes: create, write, search, open",
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
