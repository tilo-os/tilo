"""agent_task.py against a scripted desktop: the fake model, the agent loop and its checks.

No timing is asserted. The desktop is two stand-ins: a control socket that
asks the person to admit the agent and records what the run asked of it, and
a `tilo-runtime-mcp` stand-in (a script speaking MCP on stdio, as the real
client does) that waits for that admission and keeps a toy notebook behind
the door's path tools. So these prove the streamed model, the loop's turn and
time accounting, the admission, the checks that fail a run and the records it
writes, not any latency.
"""
import argparse
import json
import os
import socket
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path

import agent_task

NOTES = "so.tilo.notes"

# The door's stand-in. Its options come as JSON in AGENT_TASK_FAKE; the
# admission is a file the control socket's stand-in writes; the notebook is
# written to `state.json` when stdin closes.
FAKE_DOOR = r'''
import json, os, sys, time
options = json.loads(os.environ["AGENT_TASK_FAKE"])
shared = options["shared"]
notes = {}            # title -> {"body": [...], "finds": n}
finds_until_seen = options.get("finds_until_seen", 0)
TOOLS = [{"name": n, "description": n, "inputSchema": {"type": "object"}}
         for n in ("tilo_ls", "tilo_read", "tilo_do", "tilo_find", "tilo_status")]

def send(message):
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()

def text(ident, body, error=False):
    send({"jsonrpc": "2.0", "id": ident,
          "result": {"content": [{"type": "text", "text": body}], "isError": error}})

def note_of(path):
    for title in notes:
        if path.lower().startswith(("notes/notebook/" + title).lower()):
            return title
    return None

def find(query, app):
    if query == "new note":
        if options.get("no_notes"):
            return "nothing matches new note; tilo_ls shows the apps and what is in them"
        return "notes  Notes  running\nnotes#new-note(title?)  New note\nnotes#new-tag(name)  New tag"
    lines = []
    for title, note in notes.items():
        note["finds"] += 1
        if note["finds"] <= finds_until_seen or query.lower() not in title.lower():
            continue
        # As the door answers a note's title: the actions on the note, and
        # (where the body's label names it too) the body's.
        lines.append(f"notes/Notebook/{title}#new-child(title)  New note inside")
        lines.append(f"notes/Notebook/{title}#rename(title)  Rename note")
        if options.get("body_found"):
            lines.append(f"notes/Notebook/{title}/Body#append-paragraph(text)  Append paragraph")
    return "\n".join(lines) or f"nothing matches {query}; tilo_ls shows the apps and what is in them"

def call(name, args):
    if name == "tilo_find":
        return find(args["query"], args.get("app")), False
    if name == "tilo_ls":
        title = note_of(args["path"])
        return f"{args['path']}  notes.page\nBody/  tilo.rich-text\n#rename(value)  Rename note", title is None
    if name == "tilo_read":
        title = note_of(args["path"])
        if title is None:
            return f"nothing at {args['path']}", True
        return "# " + title + "".join("\n" + p + " {#b1}" for p in notes[title]["body"]), False
    if name == "tilo_do":
        path, action = args["path"].split("#")
        if options.get("fail"):
            return "failed: action_failed: the app refused", True
        if action == "new-note":
            notes[args["args"]["title"]] = {"body": [], "finds": 0}
            return "ok\n-> notes.page:1", False
        if action == "append-paragraph":
            title = note_of(path)
            if title is None or not path.lower().endswith("/body"):
                return f"failed: nothing named {path}; tilo_ls lists them", True
            notes[title]["body"].append(args["args"]["text"])
            return "ok\n{\"block\":\"b9\"}", False
    return f"failed: unknown tool {name}", True

for line in sys.stdin:
    message = json.loads(line)
    method, ident = message.get("method"), message.get("id")
    if method == "initialize":
        open(os.path.join(shared, "asked"), "w").close()
        while not os.path.exists(os.path.join(shared, "admitted")):
            time.sleep(0.01)
        send({"jsonrpc": "2.0", "id": ident, "result": {
            "protocolVersion": message["params"]["protocolVersion"],
            "serverInfo": {"name": "fake-door"}, "capabilities": {"tools": {}}}})
    elif method == "tools/list":
        send({"jsonrpc": "2.0", "id": ident, "result": {"tools": TOOLS}})
    elif method == "tools/call":
        body, error = call(message["params"]["name"], message["params"]["arguments"])
        text(ident, body, error)
with open(os.path.join(shared, "state.json"), "w") as out:
    json.dump(notes, out)
'''


class FakeControl:
    """The control socket: the person's prompt, their answer, grants and the scratch workspace."""

    def __init__(self, shared, ask=True):
        self.shared = shared
        self.ask = ask
        self.path = os.path.join(shared, "c.sock")
        self.requests = []
        self.workspaces = [{"id": 1, "name": "Home", "room": "room-person", "strips": []}]
        self.active = 1
        self.notes_open = False
        self.server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.server.bind(self.path)
        self.server.listen(8)
        threading.Thread(target=self.accept, daemon=True).start()

    def close(self):
        self.server.close()

    def accept(self):
        while True:
            try:
                conn, _ = self.server.accept()
            except OSError:
                return
            threading.Thread(target=self.serve, args=(conn,), daemon=True).start()

    def serve(self, conn):
        buffer = b""
        with conn:
            while True:
                chunk = conn.recv(65536)
                if not chunk:
                    return
                buffer += chunk
                while b"\n" in buffer:
                    line, _, buffer = buffer.partition(b"\n")
                    reply = self.answer(json.loads(line))
                    conn.sendall(json.dumps(reply).encode() + b"\n")

    def answer(self, payload):
        self.requests.append(payload)
        op = payload["op"]
        asked = self.ask and os.path.exists(os.path.join(self.shared, "asked"))
        if op == "actions_enumerate" and payload.get("query") == "permission-prompts":
            rows = [{"provider": "permission-prompts", "id": f"{d}@p1"}
                    for d in ("allow-once", "deny")] if asked else []
            return {"answer": {"actions": rows}}
        if op == "actions_enumerate":
            rows = [{"provider": "app-actions", "application": NOTES, "room": "room-scratch",
                     "record": "rec"}] if self.notes_open else []
            return {"answer": {"actions": rows}}
        if op == "state_dump":
            return {"answer": {"state": {"workspace_tree": {"workspaces": self.workspaces,
                                                            "active": self.active}}}}
        if op == "allow":
            return {"ok": {}}
        if op == "actions_invoke":
            address = (payload["provider"], payload["id"])
            if address == ("permission-prompts", "allow-once@p1"):
                open(os.path.join(self.shared, "admitted"), "w").close()
            elif address == ("actions", "workspace.create"):
                self.workspaces.append({"id": 2, "name": payload["args"]["name"],
                                        "room": "room-scratch", "strips": []})
                self.active = 2
            elif address == ("applications", NOTES):
                self.notes_open = True
            return {"answer": {"receipt": {"update": {"state": "succeeded"}}}}
        if op in ("windows", "list_targets"):
            return {"answer": {}}
        return {"err": {"code": "unknown_op", "message": op}}


ENV = {"commit": "0" * 40, "profile": "release", "renderer": "lavapipe",
       "virtualization": "kvm", "runner": "runner", "hardware": "test",
       "started_at": "2026-10-10T00:00:00Z"}


class AgentTaskTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.mkdtemp(prefix="atask-", dir="/tmp")
        self.runtime = Path(self.directory) / "run"
        (self.runtime / "tilo-session").mkdir(parents=True)
        (self.runtime / "tilo-session" / "agent.sock").touch()
        self.door = Path(self.directory) / "tilo-runtime-mcp"
        self.door.write_text(f"#!{sys.executable}\n{FAKE_DOOR}")
        self.door.chmod(0o755)
        self.control = None

    def tearDown(self):
        os.environ.pop("AGENT_TASK_FAKE", None)
        if self.control is not None:
            self.control.close()
        for root, dirs, files in os.walk(self.directory, topdown=False):
            for name in files:
                os.unlink(os.path.join(root, name))
            for name in dirs:
                os.rmdir(os.path.join(root, name))
        os.rmdir(self.directory)

    def run_session(self, ask=True, own_notebook=False, **fake):
        self.control = FakeControl(self.directory, ask=ask)
        os.environ["AGENT_TASK_FAKE"] = json.dumps({"shared": self.directory, **fake})
        args = argparse.Namespace(repetitions=3, warmup=1, think_ms=0.0, tokens_per_s=0.0,
                                  startup_timeout=2, own_notebook=own_notebook, mcp=self.door,
                                  client_log=None, log=None)
        return agent_task.session(self.control.path, self.runtime, args, None)

    def notebook(self):
        return json.loads((Path(self.directory) / "state.json").read_text())

    def test_the_admitted_agent_runs_every_step_through_the_door_and_is_accounted(self):
        runs = self.run_session()
        self.assertEqual(len(runs), 3)
        for run in runs:
            self.assertEqual(run.turns, 7, "six tool turns and the reply")
            self.assertEqual([call.step for call in run.calls],
                             ["discover", "create", "locate", "locate.inspect", "write", "read"])
            self.assertEqual([call.name for call in run.calls],
                             ["tilo_find", "tilo_do", "tilo_find", "tilo_ls", "tilo_do", "tilo_read"])
            self.assertGreater(run.wall_ms, 0)
            self.assertAlmostEqual(run.wall_ms, run.model_ms + run.tilo_ms + run.client_ms)
            self.assertGreaterEqual(run.client_ms, 0)
        # Warm-up and measured repetitions each wrote and read back their own note.
        notes = self.notebook()
        self.assertEqual(len(notes), 1 + 3)
        for title, note in notes.items():
            self.assertEqual(len(note["body"]), 1)
            self.assertIn(title.split()[-1], note["body"][0])
        # The person admitted it once, and granted it Notes and nothing else.
        invokes = [(r["provider"], r["id"]) for r in self.control.requests if r["op"] == "actions_invoke"]
        self.assertEqual(invokes, [("permission-prompts", "allow-once@p1")])
        grants = [r for r in self.control.requests if r["op"] == "allow"]
        self.assertEqual([(g["subject"], g["node"]) for g in grants],
                         [(f"agent:{agent_task.AGENT}", f"app:{NOTES}")])

    def test_a_note_not_yet_listed_is_searched_again_like_a_model_would(self):
        runs = self.run_session(finds_until_seen=2)
        run = runs[0]
        self.assertEqual([c.step for c in run.calls],
                         ["discover", "create", "locate", "locate.retry", "locate.retry",
                          "locate.inspect", "write", "read"])
        self.assertEqual(run.turns, 9)
        results = {kpi: samples for kpi, _, samples, _ in agent_task.collect(runs)}
        self.assertEqual(results["agent.task.notes.retries"], [2.0, 2.0, 2.0])
        meta = {kpi: meta for kpi, _, _, meta in agent_task.collect(runs)}
        self.assertEqual(meta["agent.task.notes.retries"], {"retried_locate": 6})
        locate = sum(c.ms for c in run.calls if c.step.startswith("locate"))
        self.assertAlmostEqual(results["agent.task.notes.step.locate"][0], locate)

    def test_a_body_the_search_names_is_written_straight_away(self):
        runs = self.run_session(body_found=True)
        self.assertEqual([c.step for c in runs[0].calls], list(agent_task.STEPS))
        self.assertTrue(all(len(note["body"]) == 1 for note in self.notebook().values()))

    def test_a_note_that_never_appears_fails_the_run(self):
        with self.assertRaisesRegex(agent_task.BenchError, "locate still not done"):
            self.run_session(finds_until_seen=10_000)

    def test_a_failed_action_fails_the_run(self):
        with self.assertRaisesRegex(agent_task.BenchError, "create did not succeed"):
            self.run_session(fail=True)

    def test_an_agent_the_person_is_never_asked_about_fails_the_run(self):
        with self.assertRaisesRegex(agent_task.BenchError, "never asked to admit"):
            self.run_session(ask=False)

    def test_notes_the_agent_cannot_find_fails_with_what_the_desktop_showed(self):
        with self.assertRaisesRegex(agent_task.BenchError, "never found Notes' new-note"):
            self.run_session(no_notes=True)

    def test_a_running_desktop_is_measured_in_a_workspace_of_its_own_and_left_as_found(self):
        self.run_session(own_notebook=True)
        invokes = [(r["provider"], r["id"]) for r in self.control.requests if r["op"] == "actions_invoke"]
        # Its own workspace and Notes there before the agent is admitted, and
        # the agent taken back out at the end.
        self.assertEqual(invokes, [("actions", "workspace.create"), ("applications", NOTES),
                                   ("permission-prompts", "allow-once@p1"),
                                   ("actions", "agent.remove")])

    def test_main_never_lets_a_running_desktop_use_its_notebook(self):
        seen = {}

        def session(path, runtime, args, scratch):
            seen.update(own_notebook=args.own_notebook, runtime=runtime, scratch=scratch)
            raise agent_task.BenchError("stop")

        original = agent_task.session
        agent_task.session = session
        try:
            code = agent_task.main(["--socket", "/run/user/1000/tilo-session/admin-control.sock"])
        finally:
            agent_task.session = original
        self.assertEqual(code, 1)
        self.assertEqual(seen, {"own_notebook": True, "runtime": Path("/run/user/1000"),
                                "scratch": None})

    def test_records_cover_every_step_with_provenance(self):
        runs = self.run_session()
        results = agent_task.collect(runs)
        kpis = [kpi for kpi, *_ in results]
        for step in agent_task.STEPS:
            self.assertIn(f"agent.task.notes.step.{step}", kpis)
        for kpi in ("wall", "tilo", "model", "client", "turns", "retries"):
            self.assertIn(f"agent.task.notes.{kpi}", kpis)
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory) / "task.jsonl"
            agent_task.emit(results, {"transport": "scripted"}, ENV, out)
            records = agent_task.record.read(out)
        by_kpi = {item.kpi: item for item in records}
        self.assertEqual(by_kpi["agent.task.notes.turns"].p50, 7)
        self.assertEqual(by_kpi["agent.task.notes.wall"].n, 3)
        self.assertEqual(by_kpi["agent.task.notes.step.locate"].meta["tool"], "tilo_find")
        self.assertEqual(by_kpi["agent.task.notes.step.write"].meta["tool"], "tilo_do")
        self.assertEqual(by_kpi["agent.task.notes.step.create"].meta["transport"], "scripted")

    def test_think_time_is_the_models_and_not_tilos(self):
        self.control = FakeControl(self.directory)
        os.environ["AGENT_TASK_FAKE"] = json.dumps({"shared": self.directory})
        args = argparse.Namespace(repetitions=1, warmup=0, think_ms=30.0, tokens_per_s=0.0,
                                  startup_timeout=2, own_notebook=False, mcp=self.door,
                                  client_log=None, log=None)
        run = agent_task.session(self.control.path, self.runtime, args, None)[0]
        self.assertGreaterEqual(run.model_ms, 7 * 30)
        self.assertLess(run.tilo_ms, run.model_ms)

    def test_the_door_lines_read_as_a_model_reads_them(self):
        hits = agent_task.hits_of("notes#new-note(title?)  New note\n"
                                  "notes/Notebook/A b 1/Body#append-paragraph(text)  Append paragraph\n"
                                  "notes/Notebook/A b 1  notes.page\n#rename(value)  Rename note")
        self.assertEqual([(h.path, h.action, h.params) for h in hits],
                         [("notes", "new-note", ("title?",)),
                          ("notes/Notebook/A b 1/Body", "append-paragraph", ("text",)),
                          ("notes/Notebook/A b 1", None, ()), ("", "rename", ("value",))])
        self.assertEqual(agent_task.param_name(hits[0], "title"), "title")
        self.assertEqual(agent_task.parent_of(hits[1].path, "a B 1"), "notes/Notebook/A b 1")

    def test_the_fake_model_streams_tool_calls_as_the_messages_api_does(self):
        events = list(agent_task.sse_events(
            [agent_task.tool("tilo_do", {"path": "notes#new-note", "args": {"title": "x" * 100}})], 10))
        names = [name for name, _ in events if name not in ("pace", "think")]
        self.assertEqual(names[0], "message_start")
        self.assertEqual(names[1], "content_block_start")
        self.assertGreater(names.count("content_block_delta"), 1, "input streams in pieces")
        self.assertEqual(names[-3:], ["content_block_stop", "message_delta", "message_stop"])
        joined = "".join(data["delta"]["partial_json"] for name, data in events
                         if name == "content_block_delta")
        self.assertEqual(json.loads(joined), {"path": "notes#new-note", "args": {"title": "x" * 100}})
        stop = [data for name, data in events if name == "message_delta"][0]
        self.assertEqual(stop["delta"]["stop_reason"], "tool_use")

    def test_a_paced_model_takes_its_streaming_time(self):
        model = agent_task.FakeModel(
            lambda _messages: [{"type": "text", "text": "y" * 400}], tokens_per_s=1000)
        client = agent_task.ModelClient(model.port, [])
        try:
            start = time.perf_counter()
            blocks, stop, _ = client.turn([{"role": "user", "content": "hi"}])
            elapsed = time.perf_counter() - start
        finally:
            client.close()
            model.close()
        self.assertEqual((blocks[0]["text"], stop), ("y" * 400, "end_turn"))
        self.assertGreaterEqual(elapsed, 0.09, "400 characters at 1000 tokens/s is 100 ms")


if __name__ == "__main__":
    unittest.main()
