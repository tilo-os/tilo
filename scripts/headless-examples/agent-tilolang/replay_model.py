"""A recorded model for Tilo's Tasks agent: a loopback model endpoint answering each turn from a
recorded script instead of a provider.

A script is JSON: `{"request": ..., "title": ..., "turns": [{"expect": [...], "blocks": [...]}]}`.
Turn n answers the n-th request of the task's own loop with its blocks (text and `tool_use`).
`expect` lists text the results of the previous turn's tool calls must hold, so a replay that
drifts from what Tilo answers fails at the turn it drifts, with what Tilo said, instead of
finishing on a script that no longer fits.

The composer first asks the agent's `respond` transform whether a request is answered at once or
is work for a task; the replay answers that it is work, titled as the script says.

Two dialects, as a person's provider speaks them:

- OpenAI-compatible chat completions (`/v1/chat/completions`, streamed, and `/v1/models`): what a
  provider on this device such as LM Studio speaks. The nested session's agent reaches it as
  that preset, at its default address.
- Anthropic Messages: the fake endpoint of scripts/perf/agent_task.py.
"""

from __future__ import annotations

import http.server
import json
import socket
import sys
import threading
from pathlib import Path
from typing import Any

REPO = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO / "scripts" / "perf"))
from agent_task import FakeModel, PolicyError, tool  # noqa: E402

# What the first message of a task's own loop holds, and no transform's request does.
TASK_MARK = "Operations published right now"
MODEL = "release-replay"


def text_of(content: Any) -> str:
    """Every text a message's content holds, tool results included."""
    if isinstance(content, str):
        return content
    out = []
    for block in content or []:
        if not isinstance(block, dict):
            continue
        if isinstance(block.get("text"), str):
            out.append(block["text"])
        inner = block.get("content")
        if inner is not None:
            out.append(text_of(inner))
    return "\n".join(out)


class Replay:
    """Answers model requests from a recorded script, checking each turn's premise."""

    def __init__(self, script: dict[str, Any]):
        self.turns = script["turns"]
        self.title = script.get("title", script["request"])
        self.responds = 0
        self.requests: list[list[dict[str, Any]]] = []
        self.results: list[str] = []

    def respond(self, messages: list[dict[str, Any]]) -> list[dict[str, Any]]:
        """The blocks of the next turn for a conversation (`role`/`content` messages)."""
        if not any(TASK_MARK in text_of(message.get("content")) for message in messages):
            self.responds += 1
            return [{"type": "text", "text": json.dumps({"result": {"work": self.title}})}]
        self.requests.append(messages)
        turn = sum(1 for message in messages if message.get("role") == "assistant")
        if turn >= len(self.turns):
            raise PolicyError(f"the script has {len(self.turns)} turns; the agent asked for turn {turn + 1}")
        said = ""
        if turn:
            # The results of the last turn's calls: every message after the last assistant one.
            last = max(i for i, m in enumerate(messages) if m.get("role") == "assistant")
            said = "\n".join(text_of(m.get("content")) for m in messages[last + 1:])
        self.results.append(said)
        for expected in self.turns[turn].get("expect", []):
            if expected not in said:
                raise PolicyError(f"turn {turn + 1} expects {expected!r} in what Tilo answered: {said[:1500]!r}")
        blocks = []
        for block in self.turns[turn]["blocks"]:
            if block["type"] == "tool_use":
                blocks.append(tool(block["name"], block["input"]))
            else:
                blocks.append(dict(block))
        return blocks


class OpenAiReplay:
    """A loopback OpenAI-compatible endpoint streaming a replay's turns."""

    def __init__(self, replay: Replay, port: int = 0):
        self.replay = replay
        self.errors: list[str] = []
        model = self

        class Handler(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def setup(self) -> None:
                super().setup()
                self.connection.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)

            def log_message(self, *_args: Any) -> None:
                pass

            def send_json(self, status: int, body: dict[str, Any]) -> None:
                payload = json.dumps(body).encode()
                self.send_response(status)
                self.send_header("content-type", "application/json")
                self.send_header("content-length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

            def do_GET(self) -> None:  # noqa: N802 (http.server's name)
                if self.path.rstrip("/").endswith("/models"):
                    self.send_json(200, {"object": "list", "data": [
                        {"id": MODEL, "object": "model", "owned_by": "tilo"}]})
                else:
                    self.send_json(404, {"error": {"message": f"no {self.path}"}})

            def do_POST(self) -> None:  # noqa: N802
                length = int(self.headers.get("content-length", "0"))
                body = json.loads(self.rfile.read(length))
                try:
                    if not self.path.rstrip("/").endswith("/chat/completions"):
                        raise PolicyError(f"unexpected request {self.path}")
                    blocks = model.replay.respond(body["messages"])
                except PolicyError as error:
                    model.errors.append(str(error))
                    self.send_json(400, {"error": {"message": str(error), "type": "invalid_request_error"}})
                    return
                chunks = list(model.chunks(blocks))
                if not body.get("stream"):
                    self.send_json(200, model.whole(blocks))
                    return
                self.send_response(200)
                self.send_header("content-type", "text/event-stream")
                self.send_header("transfer-encoding", "chunked")
                self.end_headers()
                for chunk in [*chunks, "[DONE]"]:
                    data = chunk if isinstance(chunk, str) else json.dumps(chunk)
                    event = f"data: {data}\n\n".encode()
                    self.wfile.write(f"{len(event):x}\r\n".encode() + event + b"\r\n")
                    self.wfile.flush()
                self.wfile.write(b"0\r\n\r\n")
                self.wfile.flush()

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", port), Handler)
        self.server.daemon_threads = True
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    @property
    def port(self) -> int:
        return self.server.server_address[1]

    @staticmethod
    def chunk(delta: dict[str, Any], finish: str | None = None) -> dict[str, Any]:
        return {"id": "chatcmpl-replay", "object": "chat.completion.chunk", "model": MODEL,
                "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}

    def chunks(self, blocks: list[dict[str, Any]]):
        yield self.chunk({"role": "assistant", "content": ""})
        calls = 0
        for block in blocks:
            if block["type"] == "text":
                text = block["text"]
                for at in range(0, len(text), 24):
                    yield self.chunk({"content": text[at:at + 24]})
            else:
                arguments = json.dumps(block["input"], separators=(",", ":"))
                yield self.chunk({"tool_calls": [{"index": calls, "id": block["id"], "type": "function",
                                                  "function": {"name": block["name"], "arguments": ""}}]})
                for at in range(0, len(arguments), 48):
                    yield self.chunk({"tool_calls": [{"index": calls, "function": {
                        "arguments": arguments[at:at + 48]}}]})
                calls += 1
        last = self.chunk({}, "tool_calls" if calls else "stop")
        last["usage"] = {"prompt_tokens": 1000, "completion_tokens": 100, "total_tokens": 1100}
        yield last

    @staticmethod
    def whole(blocks: list[dict[str, Any]]) -> dict[str, Any]:
        text = "".join(b["text"] for b in blocks if b["type"] == "text")
        calls = [{"id": b["id"], "type": "function", "function": {
            "name": b["name"], "arguments": json.dumps(b["input"])}} for b in blocks if b["type"] == "tool_use"]
        message: dict[str, Any] = {"role": "assistant", "content": text or None}
        if calls:
            message["tool_calls"] = calls
        return {"id": "chatcmpl-replay", "object": "chat.completion", "model": MODEL,
                "choices": [{"index": 0, "message": message,
                             "finish_reason": "tool_calls" if calls else "stop"}],
                "usage": {"prompt_tokens": 1000, "completion_tokens": 100, "total_tokens": 1100}}

    def close(self) -> None:
        self.server.shutdown()
        self.server.server_close()


def serve(script_path: Path, dialect: str = "openai", port: int = 0):
    """The endpoint and its replay of the script at `script_path`."""
    replay = Replay(json.loads(script_path.read_text()))
    if dialect == "anthropic":
        return FakeModel(replay.respond), replay
    return OpenAiReplay(replay, port), replay


if __name__ == "__main__":
    # Serve a script by hand: replay_model.py SCRIPT [PORT]
    endpoint, _ = serve(Path(sys.argv[1]), port=int(sys.argv[2]) if len(sys.argv) > 2 else 0)
    print(f"serving {sys.argv[1]} on 127.0.0.1:{endpoint.port}", flush=True)
    threading.Event().wait()
