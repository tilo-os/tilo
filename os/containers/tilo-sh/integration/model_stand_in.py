#!/usr/bin/env python3
"""A stand-in for a model provider that speaks Anthropic's Messages protocol.

A check that needs a model's answer and may hold no key starts this on a
loopback address and names it where the provider's address belongs. It thinks
nothing: what it answers is decided when it starts and is the same every time.

  one sentence     every question is answered with the sentence `--says`;
  one tool call    with `--tool NAME=JSON`, a question that offers a tool of
                   that name and holds no tool's answer yet is answered with
                   one call of it (the first named tool the question offers);
                   the question that brings the tool's answer back is answered
                   with the sentence `--then`, and that ends the turn.

It answers as the provider does, whole or as the provider's stream of events
(`"stream": true`): `POST /v1/messages`, and `POST /v1/messages/count_tokens`
with a size. Anything else under its address is answered 404 in the
provider's words. So a program that is started with nothing but an address
for its model - a coding program in a machine, a server's turn of an
assistant - finishes one turn against it.

Two more things, for a check that holds a whole program to it:

  `/mcp`           the least an MCP server says: it is there and has no tools,
                   for a program that is told Tilo's address beside its
                   model's and has no Tilo to ask;
  passing on       named as a program's proxy it passes nothing on: a request
                   for any other address (`CONNECT`, or a whole URL in the
                   request line) is refused and the address written down, so a
                   check learns by name what the program tried to reach.

It writes one JSON line for each request into `--notes` (and keeps them in
memory for a caller in the same process): the path, whether a key header was
there and by which name, what was asked and what it answered. Never the value
of any header, and nothing of what the question said.

usage: model_stand_in.py [--port N] [--bind ADDRESS] [--notes FILE] [--ready FILE]
                         [--says SENTENCE] [--tool NAME=JSON ...] [--then SENTENCE]

`--port 0` (the default) takes a free port; `--ready FILE` gets one JSON line
with the address once it listens. In the same process: `start(Script(...))`.
Python's own library only, so a guest image's python3 is enough.
"""

from __future__ import annotations

import argparse
import json
import os
import signal
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlsplit

SAYS = "The stand-in model answers: no model thought about this."
THEN = "The stand-in model read the tool's answer and ends here."
# The headers a key travels in. Their names are noted, their values never.
KEY_HEADERS = ("x-api-key", "authorization", "proxy-authorization")
LOOPBACK = ("127.0.0.1", "localhost", "::1")
MCP_VERSION = "2025-06-18"


class Script:
    """What the stand-in answers, decided once."""

    def __init__(self, says: str = SAYS, tools: list[tuple[str, dict]] | None = None, then: str = THEN):
        self.says = says
        self.tools = list(tools or [])
        self.then = then

    def answer(self, question: dict) -> tuple[str, list[dict], str]:
        """What `question` gets: a word for the notes, the answer's blocks
        and why the answer stops."""
        if brings_a_tools_answer(question):
            return "then", [{"type": "text", "text": self.then}], "end_turn"
        offered = offered_tools(question)
        for name, wanted in self.tools:
            if name in offered:
                call = {"type": "tool_use", "id": f"toolu_stand_in_{name.lower()}", "name": name, "input": wanted}
                return f"tool:{name}", [call], "tool_use"
        return "says", [{"type": "text", "text": self.says}], "end_turn"


def offered_tools(question: dict) -> list[str]:
    tools = question.get("tools")
    return [str(tool.get("name")) for tool in tools if isinstance(tool, dict)] if isinstance(tools, list) else []


def brings_a_tools_answer(question: dict) -> bool:
    messages = question.get("messages")
    for message in messages if isinstance(messages, list) else []:
        content = message.get("content") if isinstance(message, dict) else None
        for block in content if isinstance(content, list) else []:
            if isinstance(block, dict) and block.get("type") == "tool_result":
                return True
    return False


def whole_message(number: int, model: str, blocks: list[dict], stop: str, asked_bytes: int) -> dict:
    """A whole answer in the provider's shape."""
    said = sum(len(json.dumps(block)) for block in blocks)
    return {
        "id": f"msg_stand_in_{number:04d}",
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": blocks,
        "stop_reason": stop,
        "stop_sequence": None,
        "usage": {
            "input_tokens": max(1, asked_bytes // 4),
            "cache_read_input_tokens": 0,
            "cache_creation_input_tokens": 0,
            "output_tokens": max(1, said // 4),
        },
    }


def event(name: str, data: dict) -> str:
    return f"event: {name}\ndata: {json.dumps(data)}\n\n"


def events(message: dict) -> bytes:
    """The provider's stream of events that says `message` from its start to
    its stop: each block opened, said in one piece and closed."""
    started = dict(message, content=[], stop_reason=None, usage=dict(message["usage"], output_tokens=0))
    stream = event("message_start", {"type": "message_start", "message": started})
    for index, block in enumerate(message["content"]):
        if block["type"] == "tool_use":
            opened = dict(block, input={})
            delta = {"type": "input_json_delta", "partial_json": json.dumps(block["input"])}
        else:
            opened = {"type": "text", "text": ""}
            delta = {"type": "text_delta", "text": block["text"]}
        stream += event("content_block_start", {"type": "content_block_start", "index": index, "content_block": opened})
        stream += event("content_block_delta", {"type": "content_block_delta", "index": index, "delta": delta})
        stream += event("content_block_stop", {"type": "content_block_stop", "index": index})
    stream += event(
        "message_delta",
        {
            "type": "message_delta",
            "delta": {"stop_reason": message["stop_reason"], "stop_sequence": None},
            "usage": {"output_tokens": message["usage"]["output_tokens"]},
        },
    )
    stream += event("message_stop", {"type": "message_stop"})
    return stream.encode()


def failure(kind: str, words: str) -> bytes:
    """A failure in the provider's shape."""
    return json.dumps({"type": "error", "error": {"type": kind, "message": words}}).encode()


def mcp_answer(asked: object) -> object:
    """What the least MCP server says to one call, or to a list of them:
    None where nothing is answered (a notification)."""
    if isinstance(asked, list):
        answers = [answer for answer in (mcp_answer(one) for one in asked) if answer is not None]
        return answers or None
    if not isinstance(asked, dict) or asked.get("id") is None:
        return None
    method = asked.get("method")
    if method == "initialize":
        params = asked.get("params") if isinstance(asked.get("params"), dict) else {}
        result: dict = {
            "protocolVersion": params.get("protocolVersion") or MCP_VERSION,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "tilo-stand-in", "version": "0"},
        }
    elif method == "tools/list":
        result = {"tools": []}
    elif method == "ping":
        result = {}
    else:
        error = {"code": -32601, "message": f"the stand-in has no {method}"}
        return {"jsonrpc": "2.0", "id": asked["id"], "error": error}
    return {"jsonrpc": "2.0", "id": asked["id"], "result": result}


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "model-stand-in"
    sys_version = ""

    def log_message(self, *_: object) -> None:
        """The notes are the log."""

    def do_POST(self) -> None:
        self.answer()

    do_GET = do_PUT = do_DELETE = do_HEAD = do_OPTIONS = do_PATCH = do_POST

    def do_CONNECT(self) -> None:
        self.note(asked="pass-on", answered="refused", tried=self.path)
        self.reply(502, failure("api_error", "the stand-in passes nothing on"), close=True)

    def body(self) -> bytes:
        if "chunked" in (self.headers.get("Transfer-Encoding") or "").lower():
            pieces = []
            while True:
                size = int(self.rfile.readline().split(b";")[0].strip() or b"0", 16)
                if size == 0:
                    while self.rfile.readline().strip():
                        pass
                    return b"".join(pieces)
                pieces.append(self.rfile.read(size))
                self.rfile.readline()
        length = int(self.headers.get("Content-Length") or 0)
        return self.rfile.read(length) if length else b""

    def note(self, **what: object) -> None:
        keys = [name for name in KEY_HEADERS if self.headers.get(name) is not None]
        self.server.stand_in.note(  # type: ignore[attr-defined]
            dict(method=self.command, path=self.path, key_header=bool(keys), key_headers=keys, **what)
        )

    def reply(self, status: int, body: bytes, kind: str = "application/json", close: bool = False) -> None:
        self.send_response(status)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(body)))
        if kind == "text/event-stream":
            self.send_header("Cache-Control", "no-cache")
        if close:
            self.send_header("Connection", "close")
            self.close_connection = True
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)
        self.wfile.flush()

    def answer(self) -> None:
        raw = self.body()
        target = self.path
        if "://" in target.split("?")[0]:
            # A whole URL in the request line: this is asked as a proxy. Its
            # own address it answers itself; any other it refuses and notes.
            asked = urlsplit(target)
            port = asked.port or (443 if asked.scheme == "https" else 80)
            if asked.hostname not in LOOPBACK or port != self.server.server_port:
                self.note(asked="pass-on", answered="refused", tried=f"{asked.hostname}:{port}")
                self.reply(502, failure("api_error", "the stand-in passes nothing on"), close=True)
                return
            target = asked.path + (f"?{asked.query}" if asked.query else "")
        route = target.split("?")[0].rstrip("/")
        if route == "/v1/messages" and self.command == "POST":
            self.message(raw)
        elif route == "/v1/messages/count_tokens" and self.command == "POST":
            self.note(asked="count", answered="count")
            self.reply(200, json.dumps({"input_tokens": max(1, len(raw) // 4)}).encode())
        elif route == "/mcp":
            self.mcp(raw)
        else:
            self.note(asked="other", answered="not-found")
            self.reply(404, failure("not_found_error", f"the stand-in answers nothing at {route}"))

    def message(self, raw: bytes) -> None:
        try:
            question = json.loads(raw)
            if not isinstance(question, dict):
                raise ValueError("a question is a JSON object")
        except ValueError as why:
            self.note(asked="message", answered="not-read")
            self.reply(400, failure("invalid_request_error", f"the stand-in could not read the question: {why}"))
            return
        stand_in = self.server.stand_in  # type: ignore[attr-defined]
        which, blocks, stop = stand_in.script.answer(question)
        offered = offered_tools(question)
        streamed = question.get("stream") is True
        said = whole_message(stand_in.next_number(), str(question.get("model") or ""), blocks, stop, len(raw))
        self.note(
            asked="message",
            answered=which,
            model=said["model"],
            stream=streamed,
            messages=len(question.get("messages") or []),
            tools=len(offered),
            offers={name: name in offered for name, _ in stand_in.script.tools},
        )
        if streamed:
            self.reply(200, events(said), kind="text/event-stream")
        else:
            self.reply(200, json.dumps(said).encode())

    def mcp(self, raw: bytes) -> None:
        if self.command != "POST":
            # It keeps no stream open and no session to end.
            self.note(asked="mcp", answered="post-only")
            self.send_response(405)
            self.send_header("Allow", "POST")
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        try:
            asked = json.loads(raw)
        except ValueError:
            self.note(asked="mcp", answered="not-read")
            self.reply(400, failure("invalid_request_error", "the stand-in could not read the call"))
            return
        methods = [str(one.get("method")) for one in (asked if isinstance(asked, list) else [asked]) if isinstance(one, dict)]
        answer = mcp_answer(asked)
        self.note(asked="mcp", answered=",".join(methods)[:120])
        if answer is None:
            self.send_response(202)
            self.send_header("Content-Length", "0")
            self.end_headers()
        else:
            self.reply(200, json.dumps(answer).encode())


class StandIn:
    """A stand-in that listens: its address, what it noted, and its end."""

    def __init__(self, script: Script, port: int, notes: str | None, bind: str):
        self.script = script
        self.notes: list[dict] = []
        self._file = notes
        self._lock = threading.Lock()
        self._number = 0
        self._began = time.monotonic()
        self._server = ThreadingHTTPServer((bind, port), Handler)
        self._server.daemon_threads = True
        self._server.stand_in = self  # type: ignore[attr-defined]
        self.port = self._server.server_port
        self.url = f"http://{bind}:{self.port}"
        self._thread = threading.Thread(target=self._server.serve_forever, daemon=True)
        self._thread.start()

    def next_number(self) -> int:
        with self._lock:
            self._number += 1
            return self._number

    def note(self, what: dict) -> None:
        with self._lock:
            what = dict(n=len(self.notes) + 1, ms=int((time.monotonic() - self._began) * 1000), **what)
            self.notes.append(what)
            if self._file:
                with open(self._file, "a") as out:
                    out.write(json.dumps(what, sort_keys=True) + "\n")

    def tried(self) -> list[str]:
        """The other addresses it was asked to pass a request on to, each once."""
        with self._lock:
            return sorted({str(note["tried"]) for note in self.notes if note.get("tried")})

    def stop(self) -> None:
        self._server.shutdown()
        self._server.server_close()
        self._thread.join(timeout=5)


def start(script: Script | None = None, port: int = 0, notes: str | None = None, bind: str = "127.0.0.1") -> StandIn:
    """A stand-in listening on `bind`, answering by `script`."""
    return StandIn(script or Script(), port, notes, bind)


def named_tool(text: str) -> tuple[str, dict]:
    name, _, wanted = text.partition("=")
    value = json.loads(wanted) if wanted else {}
    if not name or not isinstance(value, dict):
        raise argparse.ArgumentTypeError("--tool takes NAME=JSON, the JSON an object")
    return name, value


def main() -> int:
    parser = argparse.ArgumentParser(description="A stand-in for a model provider (Anthropic's Messages protocol).")
    parser.add_argument("--port", type=int, default=0)
    parser.add_argument("--bind", default="127.0.0.1")
    parser.add_argument("--notes")
    parser.add_argument("--ready")
    parser.add_argument("--says", default=SAYS)
    parser.add_argument("--tool", type=named_tool, action="append", default=[])
    parser.add_argument("--then", default=THEN)
    args = parser.parse_args()
    stand_in = start(Script(args.says, args.tool, args.then), args.port, args.notes, args.bind)
    if args.ready:
        with open(args.ready + ".part", "w") as out:
            out.write(json.dumps({"url": stand_in.url, "port": stand_in.port}) + "\n")
        os.replace(args.ready + ".part", args.ready)
    print(f"the stand-in listens on {stand_in.url}", flush=True)
    ended = threading.Event()
    for one in (signal.SIGTERM, signal.SIGINT):
        signal.signal(one, lambda *_: ended.set())
    ended.wait()
    stand_in.stop()
    return 0


if __name__ == "__main__":
    sys.exit(main())
