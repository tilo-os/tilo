"""agent_task.py against a scripted Notes desktop: the fake model, the loop and its checks.

No timing is asserted. The compositor is a stand-in that speaks the control
socket's wire format and keeps a toy notebook, so these prove the streamed
model, the agent loop's turn and time accounting, the checks that fail a
run and the records it writes, not any latency.
"""
import argparse
import json
import os
import socket
import tempfile
import threading
import time
import unittest
from pathlib import Path

import agent_task

NOTES = "so.tilo.notes"


class FakeNotesDesktop:
    """A control socket over a toy notebook: new-page, append-paragraph, search, open."""

    def __init__(self, invoke_state="succeeded", unsettled=False, publish_body=True):
        self.directory = tempfile.mkdtemp(prefix="atask-", dir="/tmp")
        self.path = os.path.join(self.directory, "s.sock")
        self.invoke_state = invoke_state
        self.unsettled = unsettled
        self.publish_body = publish_body
        self.pages = {}  # page id -> {"title", "body"}
        self.opened = []
        self.requests = []
        self.jobs = {}
        self.lock = threading.Lock()
        self.server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.server.bind(self.path)
        self.server.listen(8)
        threading.Thread(target=self.accept, daemon=True).start()

    def close(self):
        self.server.close()
        os.unlink(self.path)
        os.rmdir(self.directory)

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
                    with self.lock:
                        reply = self.answer(json.loads(line))
                    conn.sendall(json.dumps(reply).encode() + b"\n")

    def rows(self):
        rows = [{"provider": "app-actions", "id": f"{NOTES}\x1fnotebook\x1fnew-page",
                 "title": "New note", "application": NOTES, "resource": "notes.notebook",
                 "operation": "new-page", "params": [{"name": "value", "type": "text",
                                                      "required": True}]},
                {"provider": "actions", "id": "navigation.axis.disable", "title": "Axis off"}]
        for page, note in self.pages.items():
            # As the desktop lists a bound resource: its id hex-encoded.
            rows.append({"provider": "app-actions", "id": f"{NOTES}\x1fpage:{page}\x1fopen",
                         "title": "Open note", "subtitle": note["title"], "application": NOTES,
                         "resource": bound(f"notes.page:{page}"), "operation": "open",
                         "params": []})
            rows.append({"provider": "app-actions", "id": f"{NOTES}\x1fpage/{page}/body\x1fappend",
                         "title": "Append paragraph", "subtitle": note["title"],
                         "application": NOTES, "resource": bound(f"notes.notebook/page/{page}/body"),
                         "operation": "append-paragraph" if self.publish_body else "bold",
                         "params": [{"name": "text", "type": "text", "required": True}]})
            for paragraph in note["body"]:
                rows.append({"provider": "app-search", "id": f"{NOTES}:block:{page}",
                             "title": paragraph, "application": NOTES,
                             "resource": f"notes.block:{page}", "operation": "open", "params": []})
        return rows

    def answer(self, payload):
        self.requests.append(payload)
        op = payload["op"]
        if op == "actions_enumerate":
            query = payload.get("query", "").lower()
            # Like the palette, a query matches more than the one row asked for.
            rows = [r for r in self.rows()
                    if not query or query in json.dumps(r).lower() or r["provider"] == "actions"]
            # Pages of one row, to prove the client follows the cursor.
            start = int(payload.get("cursor") or 0)
            answer = {"query": query, "actions": rows[start:start + 1], "settled": True}
            if start + 1 < len(rows):
                answer["next_cursor"] = str(start + 1)
            return {"answer": answer}
        if op == "actions_invoke":
            return self.invoke(payload)
        if op == "actions_status":
            job = self.jobs[payload["invocation_id"]]
            return {"answer": {"receipt": {"broker_invocation_id": payload["invocation_id"],
                                           "update": {"state": job}}}}
        return {"err": {"code": "unknown_op", "message": op}}

    def invoke(self, payload):
        ident, args = payload["id"], payload.get("args", {})
        if ident.endswith("new-page"):
            self.pages[f"{len(self.pages):08d}-0000-0000-0000-000000000000"] = {
                "title": args["value"], "body": []}
        elif ident.endswith("append"):
            page = ident.split("page/")[1].split("/")[0]
            self.pages[page]["body"].append(args["text"])
        elif ":block:" in ident:
            self.opened.append(ident)
        state = self.invoke_state
        if "wait_ms" not in payload:
            job = len(self.jobs) + 1
            self.jobs[job] = state
            return {"answer": {"receipt": {"broker_invocation_id": job,
                                           "update": {"state": "accepted"}}}}
        if self.unsettled:
            state = "running"
        return {"answer": {"receipt": {"update": {"state": state}}}}


def bound(resource):
    return f"tilo-ui-bound:v1:room:record:{resource.encode().hex()}"


def arguments(**overrides):
    values = dict(repetitions=3, warmup=1, split_repetitions=2, think_ms=0.0, tokens_per_s=0.0,
                  startup_timeout=5)
    values.update(overrides)
    return argparse.Namespace(**values)


ENV = {"commit": "0" * 40, "profile": "release", "renderer": "lavapipe",
       "virtualization": "kvm", "runner": "runner", "hardware": "test",
       "started_at": "2026-10-10T00:00:00Z"}


class AgentTaskTests(unittest.TestCase):
    def setUp(self):
        self.fake = None

    def tearDown(self):
        if self.fake is not None:
            self.fake.close()

    def desktop(self, **kwargs):
        self.fake = FakeNotesDesktop(**kwargs)
        return self.fake

    def test_the_task_runs_every_step_through_the_socket_and_is_accounted(self):
        fake = self.desktop()
        runs, split_runs = agent_task.measure(fake.path, arguments())
        self.assertEqual((len(runs), len(split_runs)), (3, 2))
        for run in runs + split_runs:
            self.assertEqual(run.turns, 7, "six tool turns and the reply")
            self.assertEqual([call.step for call in run.calls], list(agent_task.STEPS))
            self.assertGreater(run.wall_ms, 0)
            self.assertAlmostEqual(run.wall_ms, run.model_ms + run.tilo_ms + run.client_ms)
            self.assertGreaterEqual(run.client_ms, 0)
        # Warm-up, measured and split repetitions each wrote and found their own note.
        self.assertEqual(len(fake.pages), 1 + 3 + 2)
        for note in fake.pages.values():
            self.assertEqual(len(note["body"]), 1)
            self.assertIn(note["title"].split()[-1], note["body"][0])
        self.assertEqual(len(fake.opened), 6)
        enumerations = [r for r in fake.requests if r["op"] == "actions_enumerate"]
        self.assertTrue(all(r.get("detail") == "compact" for r in enumerations))
        self.assertTrue(any("cursor" in r for r in enumerations), "listings are paged through")
        first_pages = [r for r in enumerations if "cursor" not in r]
        self.assertTrue(all(r.get("wait_ms") == agent_task.WAIT_MS for r in first_pages))
        # The KPI runs settle in one round trip; the split runs accept, then wait on status.
        invokes = [r for r in fake.requests if r["op"] == "actions_invoke"]
        self.assertEqual(sum("wait_ms" in r for r in invokes), (1 + 3) * 3)
        self.assertEqual(sum(r["op"] == "actions_status" for r in fake.requests), 2 * 3)
        for run in split_runs:
            for call in run.calls:
                if call.step in agent_task.INVOKE_STEPS:
                    self.assertIsNotNone(call.accept_ms)
                    self.assertAlmostEqual(call.ms, call.accept_ms + call.apply_ms)

    def test_think_time_is_the_models_and_not_tilos(self):
        fake = self.desktop()
        runs, _ = agent_task.measure(fake.path, arguments(repetitions=1, warmup=0,
                                                          split_repetitions=0, think_ms=30))
        run = runs[0]
        self.assertGreaterEqual(run.model_ms, 7 * 30)
        self.assertLess(run.tilo_ms, run.model_ms)

    def test_records_cover_every_step_with_provenance(self):
        fake = self.desktop()
        runs, split_runs = agent_task.measure(fake.path, arguments())
        results = agent_task.collect(runs, split_runs)
        kpis = [kpi for kpi, *_ in results]
        for step in agent_task.STEPS:
            self.assertIn(f"agent.task.notes.step.{step}", kpis)
        for step in agent_task.INVOKE_STEPS:
            self.assertIn(f"agent.task.notes.step.{step}.accept", kpis)
            self.assertIn(f"agent.task.notes.step.{step}.apply", kpis)
        for kpi in ("wall", "tilo", "model", "client", "turns"):
            self.assertIn(f"agent.task.notes.{kpi}", kpis)
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory) / "task.jsonl"
            agent_task.emit(results, {"transport": "scripted"}, ENV, out)
            records = agent_task.record.read(out)
        by_kpi = {item.kpi: item for item in records}
        self.assertEqual(by_kpi["agent.task.notes.turns"].p50, 7)
        self.assertEqual(by_kpi["agent.task.notes.wall"].n, 3)
        self.assertEqual(by_kpi["agent.task.notes.step.search"].meta["tool"],
                         "tilo_enumerate_actions")
        self.assertEqual(by_kpi["agent.task.notes.step.create"].meta["transport"], "scripted")

    def test_a_failed_action_fails_the_run(self):
        fake = self.desktop(invoke_state="failed")
        with self.assertRaisesRegex(agent_task.BenchError, "ended failed"):
            agent_task.measure(fake.path, arguments(split_repetitions=0))

    def test_a_wait_that_answers_unsettled_fails_the_run(self):
        fake = self.desktop(unsettled=True)
        with self.assertRaisesRegex(agent_task.BenchError, "before the invocation settled"):
            agent_task.measure(fake.path, arguments(split_repetitions=0))

    def test_a_step_the_model_cannot_find_fails_with_what_was_listed(self):
        fake = self.desktop(publish_body=False)
        with self.assertRaisesRegex(agent_task.BenchError, "no single append-paragraph"):
            agent_task.measure(fake.path, arguments(split_repetitions=0))

    def test_the_fake_model_streams_tool_calls_as_the_messages_api_does(self):
        events = list(agent_task.sse_events(
            [agent_task.tool("tilo_invoke_action", {"provider": "p", "id": "x" * 100})], 10))
        names = [name for name, _ in events if name not in ("pace", "think")]
        self.assertEqual(names[0], "message_start")
        self.assertEqual(names[1], "content_block_start")
        self.assertGreater(names.count("content_block_delta"), 1, "input streams in pieces")
        self.assertEqual(names[-3:], ["content_block_stop", "message_delta", "message_stop"])
        joined = "".join(data["delta"]["partial_json"] for name, data in events
                         if name == "content_block_delta")
        self.assertEqual(json.loads(joined), {"provider": "p", "id": "x" * 100})
        stop = [data for name, data in events if name == "message_delta"][0]
        self.assertEqual(stop["delta"]["stop_reason"], "tool_use")

    def test_a_paced_model_takes_its_streaming_time(self):
        model = agent_task.FakeModel(
            lambda _messages: [{"type": "text", "text": "y" * 400}], tokens_per_s=1000)
        client = agent_task.ModelClient(model.port)
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
