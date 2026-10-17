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
USER_ROOM = "0f1e2d3c-4b5a-4968-8776-655443322110"
SCRATCH_ROOM = "6b70142a-2ef8-4c3c-8e2c-a385611ab642"


class FakeNotesDesktop:
    """A control socket over a toy notebook: new-page, append-paragraph, search, open."""

    def __init__(self, invoke_state="succeeded", unsettled=False, publish_body=True,
                 stale_writes=0, person=False):
        self.directory = tempfile.mkdtemp(prefix="atask-", dir="/tmp")
        self.path = os.path.join(self.directory, "s.sock")
        self.invoke_state = invoke_state
        # Refuse this many body writes as gone, as a directory that moved on does.
        self.stale_writes = stale_writes
        self.unsettled = unsettled
        self.publish_body = publish_body
        # A person's running desktop: their workspace with their notebook open.
        # Notes reaches the toy notebook only once the run opened it in a
        # workspace of its own.
        self.person = person
        self.workspaces = [{"id": 1, "name": "Home", "room": USER_ROOM, "strips": []}]
        self.active = 1
        self.launched = not person
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
        rows = self.notebook_rows() if self.launched else []
        if self.person:
            rows.append({"provider": "app-actions", "id": f"{NOTES}\x1fuser\x1fnew-page",
                         "title": "New note", "application": NOTES, "room": USER_ROOM,
                         "record": "package-primary", "resource": "notes.notebook",
                         "operation": "new-page", "params": [{"name": "value", "type": "text",
                                                              "required": True}]})
        return rows

    def notebook_rows(self):
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
                # As Notes lists a block hit: its `open`, the text matched.
                rows.append({"provider": "app-actions", "id": f"{NOTES}:block:{page}",
                             "title": "Reveal block", "subtitle": paragraph, "application": NOTES,
                             "resource": bound(f"notes.block:{page}:genesis.0"),
                             "operation": "open", "params": []})
        if self.person:
            for row in rows:
                if row.get("application") == NOTES:
                    row.update(room=SCRATCH_ROOM, record="package-primary")
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
        if op == "state_dump":
            return {"state": {"workspace_tree": {"workspaces": self.workspaces,
                                                 "active": self.active}}}
        if op == "actions_invoke" and payload["provider"] in ("actions", "applications"):
            if (payload["provider"], payload["id"]) == ("actions", "workspace.create"):
                self.workspaces.append({"id": 2, "name": payload["args"]["name"],
                                        "room": SCRATCH_ROOM, "strips": []})
                self.active = 2
            elif (payload["provider"], payload["id"]) == ("applications", NOTES):
                self.launched = True
            return {"answer": {"receipt": {"update": {"state": "succeeded"}}}}
        if op == "actions_invoke":
            return self.invoke(payload)
        if op == "actions_status":
            job = self.jobs[payload["invocation_id"]]
            return {"answer": {"receipt": {"broker_invocation_id": payload["invocation_id"],
                                           "update": {"state": job}}}}
        return {"err": {"code": "unknown_op", "message": op}}

    def invoke(self, payload):
        ident, args = payload["id"], payload.get("args", {})
        if "\x1fuser\x1f" in ident:
            return {"err": {"code": "test", "message": "the person's notebook was written to"}}
        if ident.endswith("append") and self.stale_writes:
            self.stale_writes -= 1
            return {"err": {"code": "action_failed", "message": "directory action refused: invalid "
                            "request: the action changed since that revision; list it again"}}
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
    # The fake is a throwaway desktop, as `--spawn` seeds; `own_notebook`
    # is what `--socket` against a person's desktop runs with.
    values = dict(repetitions=3, warmup=1, split_repetitions=2, think_ms=0.0, tokens_per_s=0.0,
                  startup_timeout=5, own_notebook=False)
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
        # The startup probe waits for Notes; the agent's searches wait as long
        # as a search may.
        self.assertTrue(all(r.get("wait_ms") in (agent_task.LISTING_WAIT_MS, agent_task.WAIT_MS)
                            for r in first_pages))
        self.assertTrue(any(r.get("wait_ms") == agent_task.LISTING_WAIT_MS for r in first_pages))
        # The KPI runs settle in one round trip; the split runs accept, then wait on status.
        invokes = [r for r in fake.requests if r["op"] == "actions_invoke"]
        self.assertEqual(sum("wait_ms" in r for r in invokes), (1 + 3) * 3)
        self.assertEqual(sum(r["op"] == "actions_status" for r in fake.requests), 2 * 3)
        for run in split_runs:
            for call in run.calls:
                if call.step in agent_task.INVOKE_STEPS:
                    self.assertIsNotNone(call.accept_ms)
                    self.assertAlmostEqual(call.ms, call.accept_ms + call.apply_ms)

    def test_a_running_desktop_is_measured_in_a_notebook_of_its_own(self):
        fake = self.desktop(person=True)
        runs, _ = agent_task.measure(fake.path, arguments(repetitions=1, warmup=0,
                                                          split_repetitions=0, own_notebook=True))
        self.assertEqual(len(runs), 1)
        invokes = [(r["provider"], r["id"]) for r in fake.requests if r["op"] == "actions_invoke"]
        # Its own workspace, Notes opened there, then only that notebook.
        self.assertEqual(invokes[:2], [("actions", "workspace.create"), ("applications", NOTES)])
        self.assertTrue(all("\x1fuser\x1f" not in ident for _, ident in invokes))
        self.assertEqual(len(fake.pages), 1)

    def test_main_never_lets_a_running_desktop_use_its_notebook(self):
        seen = {}

        def measure(path, args):
            seen["own_notebook"] = args.own_notebook
            raise agent_task.BenchError("stop")

        original = agent_task.measure
        agent_task.measure = measure
        try:
            self.assertEqual(agent_task.main(["--socket", "/nonexistent"]), 1)
        finally:
            agent_task.measure = original
        self.assertTrue(seen["own_notebook"])

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

    def test_a_refused_stale_address_is_listed_again_and_retried_like_a_model_would(self):
        fake = self.desktop(stale_writes=2)
        runs, _ = agent_task.measure(fake.path, arguments(repetitions=1, warmup=0,
                                                          split_repetitions=0))
        run = runs[0]
        self.assertEqual([c.step for c in run.calls],
                         ["discover", "create", "locate", "write", "locate.retry", "write.retry",
                          "locate.retry", "write.retry", "search", "open"])
        self.assertEqual(run.turns, 11)
        note = next(iter(fake.pages.values()))
        self.assertEqual(len(note["body"]), 1, "the body is written once, by the last retry")
        results = {kpi: samples for kpi, _, samples, _ in agent_task.collect(runs, [])}
        self.assertEqual(results["agent.task.notes.retries"], [4.0])
        write = sum(c.ms for c in run.calls if c.step.startswith("write"))
        self.assertAlmostEqual(results["agent.task.notes.step.write"][0], write)

    def test_refusals_past_the_retry_bound_fail_the_run(self):
        fake = self.desktop(stale_writes=9)
        with self.assertRaisesRegex(agent_task.BenchError, "write was refused"):
            agent_task.measure(fake.path, arguments(repetitions=1, warmup=0, split_repetitions=0))

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
