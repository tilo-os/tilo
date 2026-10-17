"""Failure-path tests for the resource benchmark, without starting a desktop."""
import importlib.util
import json
from pathlib import Path
import socket
import tempfile
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location("resource_bench", Path(__file__).with_name("resource-bench.py"))
bench = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bench)

HEADER = "start_us,period_us,sleep_us,scene_us,submit_us,testctl_us,painted,wake,input_latency_us\n"
ROW = "0,100,90,2,1,1,1,testctl,\n"


class ResourceBenchTests(unittest.TestCase):
    def send_response(self, response, action):
        client, server = socket.socketpair()
        try:
            server.sendall(response)
            server.shutdown(socket.SHUT_WR)
            # The socketpair exercises send/recv and EOF handling; only connect
            # is replaced because the pair is already connected.
            class Connected:
                def __enter__(self): return self
                def __exit__(self, *args): pass
                def settimeout(self, timeout): client.settimeout(timeout)
                def connect(self, path): pass
                def sendall(self, data): client.sendall(data)
                def recv(self, size): return client.recv(size)
            with patch.object(bench.socket, "socket", return_value=Connected()):
                return action(bench.Control(Path("unused.sock")))
        finally:
            client.close()
            server.close()

    def test_workload_control_errors_retain_root_cause(self):
        response = b'{"err":{"code":"unsupported","message":"scroll unavailable"}}\n'
        with self.assertRaisesRegex(bench.ControlError, "scroll.*unsupported.*scroll unavailable"):
            self.send_response(response, lambda ctl: ctl.send({"op": "scroll", "dy": 3}))
        for response in (b"", b"{}\n", b"[]\n", b'{"ok":{"frame":0}}'):
            with self.subTest(response=response), self.assertRaises(RuntimeError):
                self.send_response(response, lambda ctl: ctl.send({"op": "key"}))
        with self.assertRaisesRegex(RuntimeError, "foreign_frames.*invalid"):
            self.send_response(b'{"ok":{"frame":0}}\n', bench.foreign_frames)
        self.assertEqual(self.send_response(b'{"foreign_frames":{"frames":[]}}\n', bench.foreign_frames), [])
        self.assertEqual(self.send_response(b'{"ok":{"frame":0}}\n',
                                          lambda ctl: ctl.send({"op": "key"})), {"ok": {"frame": 0}})

    def test_launch_preserves_application_index_retry(self):
        class Control:
            attempts = 0
            observations = 0
            def send(self, command):
                if command["op"] == "foreign_frames":
                    self.observations += 1
                    frames = [] if self.observations == 1 else [
                        {"surface_id": 1, "app_id": "example", "frame_seq": 1}]
                    return {"foreign_frames": {"frames": frames}}
                self.attempts += 1
                if self.attempts == 1:
                    raise bench.ControlError("application index not ready")
                return {"ok": {"frame": 0}}
        ctl = Control()
        with patch.object(bench.time, "sleep"):
            self.assertTrue(bench.launch(ctl, "example", 1)[0])
        self.assertEqual(ctl.attempts, 2)

    def test_trace_corruption_is_not_dropped_from_measurements(self):
        with tempfile.TemporaryDirectory() as directory:
            trace = Path(directory) / "frames.csv"
            for invalid in ("1,100\n", ROW.replace(",100,", ",bad,"),
                            ROW.replace(",1,testctl,", ",2,testctl,")):
                trace.write_text(HEADER + ROW + invalid)
                with self.subTest(invalid=invalid), self.assertRaisesRegex(ValueError, "frames.csv:3:"):
                    bench.read_trace(trace)
            trace.write_text("start_us,start_us\n0,0\n")
            with self.assertRaisesRegex(ValueError, "columns"):
                bench.read_trace(trace)
            trace.unlink()
            with self.assertRaises(FileNotFoundError):
                bench.read_trace(trace)
            trace.write_text(HEADER + ROW)
            rows = bench.read_trace(trace)
            self.assertEqual(len(rows), 1)
            self.assertEqual(rows[0]["input_latency_us"], "")  # intentionally optional
            self.assertEqual(bench.frame_stats(rows, 0, 100)["painted_s"], 10000)

    def test_failed_app_launch_fails_cli_even_with_valid_trace(self):
        for painted in (False, True):
            with self.subTest(painted=painted), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                shell = root / "shell"
                shell.touch()
                class Process:
                    pid = 123
                    def __init__(self, *args, env, **kwargs):
                        Path(env["TILO_TEST_CONTROL"]).touch()
                        Path(env["TILO_FRAME_TRACE"]).write_text(HEADER + ROW)
                class Snapshot:
                    procs = {}
                    def __init__(self, pid): pass
                argv = ["resource-bench.py", "--shell-bin", str(shell), "--out", str(root),
                        "--apps", "example", "--seconds", "0", "--settle", "0",
                        "--skip", ",".join(bench.PHASES[1:])]
                with (patch.object(bench.sys, "argv", argv),
                      patch.object(bench.subprocess, "run"),
                      patch.object(bench.subprocess, "Popen", Process),
                      patch.object(bench, "kill_group"),
                      patch.object(bench, "Control"),
                      patch.object(bench, "Snapshot", Snapshot),
                      patch.object(bench, "diff", return_value={}),
                      patch.object(bench, "gpu_drivers", return_value={}),
                      patch.object(bench, "launch", return_value=(painted, 30)),
                      patch.object(bench, "print_report"),
                      patch.object(bench.time, "sleep")):
                    self.assertEqual(bench.main(), 0 if painted else 1)
                report = json.loads((root / "run.json").read_text())
                self.assertEqual(report["apps"][0]["painted"], painted)
                self.assertEqual(len(report["violations"]), 0 if painted else 1)
                if not painted:
                    self.assertIn("example: no painted frame", report["violations"][0])


if __name__ == "__main__":
    unittest.main()
