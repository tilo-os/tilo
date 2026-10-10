"""agent_rtt.py against a scripted control socket: what it sends, checks and records.

No timing is asserted; the compositor is a stand-in that speaks the wire
format, so these prove the client, the checks that fail a run and the
records it writes, not any latency.
"""
import argparse
import json
import os
import socket
import tempfile
import threading
import unittest
from pathlib import Path

import agent_rtt


class FakeCompositor:
    """Answers the control socket's JSON lines on every connection it accepts."""

    def __init__(self, invoke_state="succeeded", wait_state="succeeded", refuse=None):
        self.directory = tempfile.mkdtemp(prefix="artt-", dir="/tmp")
        self.path = os.path.join(self.directory, "s.sock")
        self.invoke_state = invoke_state
        self.wait_state = wait_state
        self.refuse = refuse
        self.requests = []
        self.lock = threading.Lock()
        self.server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.server.bind(self.path)
        self.server.listen(32)
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
                    conn.sendall(json.dumps(self.answer(json.loads(line))).encode() + b"\n")

    def answer(self, payload):
        with self.lock:
            self.requests.append(payload)
        op = payload["op"]
        if self.refuse == op:
            return {"err": {"code": "not_permitted", "message": "scripted refusal"}}
        if op == "actions_enumerate":
            return {"answer": {"query": "", "count": 2, "total": 2, "truncated": False,
                               "actions": [{"provider": "actions", "id": "a"},
                                           {"provider": "actions", "id": "b"}]}}
        if op == "state_dump":
            return {"workspace_tree": {"workspaces": []}, "chrome": {}}
        if op == "actions_invoke":
            state = self.wait_state if "wait_ms" in payload else self.invoke_state
            return {"answer": {"action": {"provider": payload["provider"], "id": payload["id"]},
                               "receipt": {"update": {"state": state, "invocation": "x"}}}}
        return {"err": {"code": "unknown_op", "message": op}}


def arguments(**overrides):
    values = dict(samples=5, warmup=2, noop=agent_rtt.NOOP, app=None, clients=0,
                  duration=1.0, mcp=False, transport="scripted socket")
    values.update(overrides)
    return argparse.Namespace(**values)


ENV = {"commit": "0" * 40, "profile": "release", "renderer": "lavapipe",
       "virtualization": "kvm", "runner": "runner", "hardware": "test", "started_at": "2026-10-09T00:00:00Z"}


class AgentRoundTripTests(unittest.TestCase):
    def setUp(self):
        self.fake = None

    def tearDown(self):
        if self.fake is not None:
            self.fake.close()

    def compositor(self, **kwargs):
        self.fake = FakeCompositor(**kwargs)
        return self.fake

    def test_every_workload_records_its_samples_with_provenance(self):
        fake = self.compositor()
        results = agent_rtt.run(arguments(app=("app-actions", "photo/rotate-right"), clients=2,
                                          mcp=True), fake.path)
        by_kpi = {kpi: (unit, samples, meta) for kpi, unit, samples, meta in results}
        self.assertEqual(sorted(by_kpi), [
            "agent.invoke.local.app", "agent.invoke.local.app.accept",
            "agent.invoke.local.noop", "agent.invoke.local.noop.accept",
            "agent.invoke.local.throughput", "agent.invoke.mcp.noop",
            "agent.list.local", "agent.state_dump.local"])
        for kpi, (unit, samples, meta) in by_kpi.items():
            with self.subTest(kpi=kpi):
                if kpi == "agent.invoke.local.throughput":
                    self.assertEqual(unit, "us")
                    self.assertEqual(len(samples), 1)
                    self.assertEqual(meta["clients"], 2)
                    self.assertGreater(meta["calls"], 0)
                else:
                    self.assertEqual(unit, "ms")
                    self.assertEqual(len(samples), 5)
                self.assertTrue(all(sample > 0 for sample in samples))
        self.assertEqual(by_kpi["agent.list.local"][2]["rows"], 2)
        self.assertGreater(by_kpi["agent.state_dump.local"][2]["response_bytes"], 0)
        self.assertEqual(by_kpi["agent.invoke.local.app"][2]["address"], "app-actions/photo/rotate-right")

        invokes = [r for r in fake.requests if r["op"] == "actions_invoke"]
        noop = [r for r in invokes if r["id"] == agent_rtt.NOOP[1]]
        self.assertTrue(any("wait_ms" not in r for r in noop), "accept is timed without wait_ms")
        self.assertTrue(any(r.get("wait_ms") == agent_rtt.WAIT_MS for r in noop),
                        "complete is timed with wait_ms")
        # Warm-up calls are made and checked, but not recorded.
        enumerations = [r for r in fake.requests if r["op"] == "actions_enumerate"]
        self.assertEqual(enumerations[0], {"op": "actions_enumerate", "query": ""})
        self.assertEqual(len(enumerations), 2 + 5)

    def test_a_wait_that_answers_before_the_invocation_settles_fails_the_run(self):
        fake = self.compositor(wait_state="accepted")
        with self.assertRaisesRegex(agent_rtt.BenchError, "before the invocation settled"):
            agent_rtt.run(arguments(), fake.path)

    def test_an_action_that_fails_fails_the_run(self):
        fake = self.compositor(invoke_state="failed")
        with self.assertRaisesRegex(agent_rtt.BenchError, "ended failed"):
            agent_rtt.run(arguments(), fake.path)

    def test_a_refusal_fails_the_run_with_the_compositor_code(self):
        fake = self.compositor(refuse="state_dump")
        with self.assertRaisesRegex(agent_rtt.BenchError, "not_permitted"):
            agent_rtt.run(arguments(), fake.path)

    def test_a_refusal_through_mcp_fails_the_run(self):
        fake = self.compositor(refuse="actions_invoke")
        with self.assertRaisesRegex(agent_rtt.BenchError, "MCP tools/call failed"):
            agent_rtt.mcp_noop(fake.path, agent_rtt.NOOP, 0, 1)

    def test_records_carry_the_environment_and_validate(self):
        results = [("agent.invoke.local.noop", "ms", [0.4, 0.2, 0.3, 5.0], {"address": "actions/x"})]
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory) / "agent.jsonl"
            agent_rtt.emit(results, ENV, out)
            records = agent_rtt.record.read(out)
        self.assertEqual(len(records), 1)
        item = records[0]
        self.assertEqual((item.kpi, item.unit, item.n, item.p50, item.max),
                         ("agent.invoke.local.noop", "ms", 4, 0.3, 5.0))
        self.assertEqual(item.meta["address"], "actions/x")
        self.assertEqual(item.profile, "release")

    def test_group_liveness_ignores_zombies_and_other_groups(self):
        with tempfile.TemporaryDirectory() as directory:
            proc = Path(directory)
            for pid, line in {"10": "10 (shell) Z 1 77 77", "11": "11 (a) b) S 1 78 78",
                              "12": "12 (x) S 1 79 79"}.items():
                (proc / pid).mkdir()
                (proc / pid / "stat").write_text(line + " 0 0\n")
            self.assertFalse(agent_rtt.group_alive(77, proc))
            self.assertTrue(agent_rtt.group_alive(78, proc))
            self.assertFalse(agent_rtt.group_alive(80, proc))

    def test_addresses_split_on_the_first_colon(self):
        self.assertEqual(agent_rtt.parse_address("files:files.entry:/tmp/a"),
                         ("files", "files.entry:/tmp/a"))
        with self.assertRaises(argparse.ArgumentTypeError):
            agent_rtt.parse_address("no-colon")


if __name__ == "__main__":
    unittest.main()
