"""The model stand-in of the whole-stack check, held to what a program asks of a provider.

No program of the check is started here: the questions are made as a provider's client makes
them, on a loopback address, and the answers read as that client reads them.
"""

from __future__ import annotations

import http.client
import importlib.util
import json
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "os/containers/tilo-sh/integration/model_stand_in.py"
# A word in a key's place. It is made up and opens nothing anywhere.
MADE_UP_KEY = "made-up-word-of-a-test-not-a-key"


def load():
    spec = importlib.util.spec_from_file_location("model_stand_in", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


stand_in = load()

WRITE = {"file_path": "/tmp/made-for-a-test/note.txt", "content": "written for a test\n"}
SHELL = {"command": "printf written > note.txt"}


def question(**more: object) -> dict:
    asked = {
        "model": "claude-made-up",
        "max_tokens": 1024,
        "messages": [{"role": "user", "content": [{"type": "text", "text": "Say something."}]}],
    }
    asked.update(more)
    return asked


def tool(name: str) -> dict:
    return {"name": name, "description": name, "input_schema": {"type": "object"}}


def with_the_tools_answer(asked: dict, call: dict) -> dict:
    """The question a program asks next: the call it was given, and what the tool said."""
    messages = asked["messages"] + [
        {"role": "assistant", "content": [call]},
        {"role": "user", "content": [{"type": "tool_result", "tool_use_id": call["id"], "content": "done"}]},
    ]
    return dict(asked, messages=messages)


def read_events(body: bytes) -> list[tuple[str, dict]]:
    """A stream of events as a client reads it: each event's name and its data."""
    read = []
    for piece in body.decode().split("\n\n"):
        if not piece.strip():
            continue
        lines = piece.split("\n")
        name = next(line[len("event: "):] for line in lines if line.startswith("event: "))
        data = json.loads(next(line[len("data: "):] for line in lines if line.startswith("data: ")))
        read.append((name, data))
    return read


def said_by(read: list[tuple[str, dict]]) -> dict:
    """The whole answer a stream of events says, put together as a client does."""
    message = dict(next(data for name, data in read if name == "message_start")["message"])
    blocks: list[dict] = []
    pieces: dict[int, str] = {}
    for name, data in read:
        if name == "content_block_start":
            blocks.append(dict(data["content_block"]))
            pieces[data["index"]] = ""
        elif name == "content_block_delta":
            delta = data["delta"]
            pieces[data["index"]] += delta.get("text") or delta.get("partial_json") or ""
        elif name == "message_delta":
            message["stop_reason"] = data["delta"]["stop_reason"]
    for index, block in enumerate(blocks):
        if block["type"] == "tool_use":
            block["input"] = json.loads(pieces[index])
        else:
            block["text"] = pieces[index]
    message["content"] = blocks
    return message


class TheStandIn(unittest.TestCase):
    def setUp(self) -> None:
        self.folder = tempfile.TemporaryDirectory()
        self.addCleanup(self.folder.cleanup)
        self.notes = str(Path(self.folder.name) / "notes.jsonl")

    def listening(self, script=None):
        one = stand_in.start(script, notes=self.notes)
        self.addCleanup(one.stop)
        return one

    def ask(self, one, method: str, path: str, body: object = None, headers: dict | None = None, **more):
        connection = http.client.HTTPConnection("127.0.0.1", one.port, timeout=10)
        self.addCleanup(connection.close)
        sent = dict(headers or {})
        payload = None
        if body is not None:
            payload = body if isinstance(body, (bytes, list)) else json.dumps(body).encode()
            sent.setdefault("Content-Type", "application/json")
        connection.request(method, path, body=payload, headers=sent, **more)
        answer = connection.getresponse()
        return answer.status, answer.getheader("Content-Type"), answer.read()

    def test_a_streamed_question_is_answered_with_the_one_sentence_as_the_providers_events(self) -> None:
        one = self.listening()
        status, kind, body = self.ask(
            one, "POST", "/v1/messages?beta=true", question(stream=True), {"x-api-key": MADE_UP_KEY}
        )
        self.assertEqual((status, kind), (200, "text/event-stream"))
        read = read_events(body)
        self.assertEqual(
            [name for name, _ in read],
            ["message_start", "content_block_start", "content_block_delta", "content_block_stop",
             "message_delta", "message_stop"],
        )
        self.assertTrue(all(data["type"] == name for name, data in read), "each event's data names its type")
        started = read[0][1]["message"]
        self.assertEqual((started["role"], started["content"], started["stop_reason"]), ("assistant", [], None))
        self.assertEqual(started["model"], "claude-made-up", "the model is said back as it was named")
        said = said_by(read)
        self.assertEqual(said["content"], [{"type": "text", "text": stand_in.SAYS}])
        self.assertEqual(said["stop_reason"], "end_turn")

    def test_a_question_that_asks_for_no_stream_gets_the_whole_answer(self) -> None:
        one = self.listening(stand_in.Script(says="Only this."))
        status, kind, body = self.ask(one, "POST", "/v1/messages", question())
        self.assertEqual((status, kind), (200, "application/json"))
        said = json.loads(body)
        self.assertEqual((said["type"], said["role"], said["stop_reason"]), ("message", "assistant", "end_turn"))
        self.assertEqual(said["content"], [{"type": "text", "text": "Only this."}])
        self.assertGreater(said["usage"]["input_tokens"], 0)
        self.assertGreater(said["usage"]["output_tokens"], 0)

    def test_the_same_question_is_answered_the_same_each_time_under_an_id_of_its_own(self) -> None:
        one = self.listening()
        first = json.loads(self.ask(one, "POST", "/v1/messages", question())[2])
        second = json.loads(self.ask(one, "POST", "/v1/messages", question())[2])
        self.assertNotEqual(first["id"], second["id"], "a program tells two answers apart by their ids")
        self.assertEqual(first["content"], second["content"])

    def test_one_tool_call_is_asked_for_and_the_tools_answer_ends_the_turn(self) -> None:
        one = self.listening(stand_in.Script(tools=[("Write", WRITE), ("Bash", SHELL)]))
        asked = question(stream=True, tools=[tool("Read"), tool("Bash"), tool("Write")])
        said = said_by(read_events(self.ask(one, "POST", "/v1/messages", asked)[2]))
        self.assertEqual(said["stop_reason"], "tool_use")
        self.assertEqual(len(said["content"]), 1)
        call = said["content"][0]
        self.assertEqual((call["type"], call["name"], call["input"]), ("tool_use", "Write", WRITE))
        self.assertTrue(call["id"])
        after = said_by(read_events(self.ask(one, "POST", "/v1/messages", with_the_tools_answer(asked, call))[2]))
        self.assertEqual(after["stop_reason"], "end_turn")
        self.assertEqual(after["content"], [{"type": "text", "text": stand_in.THEN}])
        answered = [note["answered"] for note in one.notes]
        self.assertEqual(answered, ["tool:Write", "then"])
        self.assertEqual(one.notes[0]["offers"], {"Write": True, "Bash": True})

    def test_the_first_named_tool_a_question_offers_is_the_one_asked_for(self) -> None:
        one = self.listening(stand_in.Script(tools=[("Write", WRITE), ("Bash", SHELL)]))
        asked = question(tools=[tool("Read"), tool("Bash")])
        said = json.loads(self.ask(one, "POST", "/v1/messages", asked)[2])
        self.assertEqual((said["content"][0]["name"], said["content"][0]["input"]), ("Bash", SHELL))
        self.assertEqual(one.notes[0]["offers"], {"Write": False, "Bash": True})

    def test_a_question_that_offers_none_of_the_named_tools_gets_the_sentence(self) -> None:
        one = self.listening(stand_in.Script(tools=[("Write", WRITE)]))
        for asked in (question(), question(tools=[tool("Read")])):
            said = json.loads(self.ask(one, "POST", "/v1/messages", asked)[2])
            self.assertEqual(said["content"], [{"type": "text", "text": stand_in.SAYS}])
            self.assertEqual(said["stop_reason"], "end_turn")

    def test_each_request_is_noted_with_its_path_and_whether_a_key_came_never_the_key(self) -> None:
        one = self.listening()
        self.ask(one, "POST", "/v1/messages?beta=true", question(), {"x-api-key": MADE_UP_KEY})
        self.ask(one, "POST", "/v1/messages", question(), {"Authorization": f"Bearer {MADE_UP_KEY}"})
        self.ask(one, "POST", "/v1/messages", question())
        noted = [json.loads(line) for line in Path(self.notes).read_text().splitlines()]
        self.assertEqual(noted, one.notes, "the file holds what the caller in the same process reads")
        self.assertEqual([note["path"] for note in noted], ["/v1/messages?beta=true", "/v1/messages", "/v1/messages"])
        self.assertEqual([note["key_header"] for note in noted], [True, True, False])
        self.assertEqual([note["key_headers"] for note in noted], [["x-api-key"], ["authorization"], []])
        self.assertEqual([note["n"] for note in noted], [1, 2, 3])
        written = Path(self.notes).read_text()
        self.assertNotIn(MADE_UP_KEY, written, "a header's value is never written")
        self.assertNotIn("Say something.", written, "nor what a question said")

    def test_a_count_of_tokens_is_answered_with_a_size(self) -> None:
        one = self.listening()
        status, _, body = self.ask(one, "POST", "/v1/messages/count_tokens", question())
        self.assertEqual(status, 200)
        self.assertGreater(json.loads(body)["input_tokens"], 0)
        self.assertEqual(one.notes[0]["asked"], "count")

    def test_a_question_sent_in_pieces_is_read_whole(self) -> None:
        one = self.listening()
        raw = json.dumps(question()).encode()
        pieces = [raw[:7], raw[7:40], raw[40:]]
        status, _, body = self.ask(one, "POST", "/v1/messages", pieces, encode_chunked=True)
        self.assertEqual(status, 200)
        self.assertEqual(json.loads(body)["content"][0]["text"], stand_in.SAYS)

    def test_what_it_does_not_answer_is_refused_in_the_providers_words(self) -> None:
        one = self.listening()
        status, _, body = self.ask(one, "GET", "/v1/models")
        self.assertEqual(status, 404)
        self.assertEqual(json.loads(body)["type"], "error")
        self.assertEqual(json.loads(body)["error"]["type"], "not_found_error")
        status, _, body = self.ask(one, "POST", "/v1/messages", b"not json")
        self.assertEqual(status, 400)
        self.assertEqual(json.loads(body)["error"]["type"], "invalid_request_error")
        self.assertEqual([note["answered"] for note in one.notes], ["not-found", "not-read"])

    def test_asked_as_a_proxy_it_passes_nothing_on_and_names_what_was_tried(self) -> None:
        one = self.listening()
        status, _, _ = self.ask(one, "CONNECT", "telemetry.example.invalid:443")
        self.assertEqual(status, 502)
        status, _, _ = self.ask(one, "GET", "http://updates.example.invalid/latest")
        self.assertEqual(status, 502)
        # Its own address in a whole URL is a program that sends its model's questions
        # through the proxy it was named: answered as if asked directly.
        status, _, body = self.ask(one, "POST", f"http://127.0.0.1:{one.port}/v1/messages", question())
        self.assertEqual(status, 200)
        self.assertEqual(json.loads(body)["content"][0]["text"], stand_in.SAYS)
        self.assertEqual(one.tried(), ["telemetry.example.invalid:443", "updates.example.invalid:80"])
        self.assertEqual([note["answered"] for note in one.notes], ["refused", "refused", "says"])

    def test_the_least_an_mcp_server_says(self) -> None:
        one = self.listening()
        opening = {"jsonrpc": "2.0", "id": 1, "method": "initialize",
                   "params": {"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "a test"}}}
        status, kind, body = self.ask(one, "POST", "/mcp", opening)
        self.assertEqual((status, kind), (200, "application/json"))
        opened = json.loads(body)
        self.assertEqual((opened["id"], opened["result"]["protocolVersion"]), (1, "2025-03-26"))
        self.assertIn("tools", opened["result"]["capabilities"])
        status, _, body = self.ask(one, "POST", "/mcp", {"jsonrpc": "2.0", "method": "notifications/initialized"})
        self.assertEqual((status, body), (202, b""))
        listed = json.loads(self.ask(one, "POST", "/mcp", {"jsonrpc": "2.0", "id": 2, "method": "tools/list"})[2])
        self.assertEqual(listed["result"], {"tools": []})
        unknown = json.loads(self.ask(one, "POST", "/mcp", {"jsonrpc": "2.0", "id": 3, "method": "prompts/list"})[2])
        self.assertEqual(unknown["error"]["code"], -32601)
        self.assertEqual(self.ask(one, "GET", "/mcp")[0], 405, "it keeps no stream open")

    def test_started_as_a_program_it_says_where_it_listens_and_ends_when_told(self) -> None:
        ready = Path(self.folder.name) / "ready.json"
        program = subprocess.Popen(
            [sys.executable, str(SCRIPT), "--ready", str(ready), "--notes", self.notes,
             "--says", "Said by the program.", "--tool", "Write=" + json.dumps(WRITE)],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        self.addCleanup(program.kill)
        deadline = time.monotonic() + 20
        while not ready.exists() and time.monotonic() < deadline and program.poll() is None:
            time.sleep(0.05)
        self.assertTrue(ready.exists(), "it wrote where it listens")
        port = json.loads(ready.read_text())["port"]
        connection = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
        self.addCleanup(connection.close)
        connection.request("POST", "/v1/messages", body=json.dumps(question()).encode())
        self.assertEqual(json.loads(connection.getresponse().read())["content"][0]["text"], "Said by the program.")
        connection.request("POST", "/v1/messages", body=json.dumps(question(tools=[tool("Write")])).encode())
        self.assertEqual(json.loads(connection.getresponse().read())["content"][0]["input"], WRITE)
        connection.close()
        program.terminate()
        self.assertEqual(program.wait(timeout=10), 0)
        self.assertEqual(len(Path(self.notes).read_text().splitlines()), 2)


if __name__ == "__main__":
    unittest.main()
