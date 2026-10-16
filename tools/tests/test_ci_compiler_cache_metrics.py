import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[2]
HELPER = ROOT / "tools/ci/compiler_cache_metrics.py"
spec = importlib.util.spec_from_file_location("cache_metrics", HELPER)
metrics = importlib.util.module_from_spec(spec)
spec.loader.exec_module(metrics)


class CompilerCacheMeasurements(unittest.TestCase):
    def test_real_measure_wrapper_brackets_command_without_reset_and_keeps_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            stats = root / "stats"
            calls = root / "calls"
            client = root / "sccache"
            client.write_text("#!/usr/bin/env python3\nimport os,sys,pathlib\n"
                              "assert sys.argv[1:]==['--show-stats','--stats-format=json']\n"
                              "p=pathlib.Path(os.environ['TEST_CALLS']);p.open('a').write('read\\n')\n"
                              "print(pathlib.Path(os.environ['TEST_STATS']).read_text())\n")
            client.chmod(0o755)
            stats.write_text(json.dumps({"stats": {"cache_hits": {"counts": {"Rust": 100}},
                                                    "cache_misses": {"counts": {"Rust": 20}}},
                                         "cache_location": "secret-token-should-not-escape"}))
            command = ["python3", "-c", "import pathlib,json,sys; "
                       "pathlib.Path(sys.argv[1]).write_text(json.dumps({'stats': "
                       "{'cache_hits': {'counts': {'Rust': 117}}, 'cache_misses': "
                       "{'counts': {'Rust': 24}}}, 'cache_location': 'private-endpoint'})); "
                       "sys.exit(7)", str(stats)]
            result = subprocess.run(["bash", str(ROOT / "tools/ci/measure.sh"), "cache fixture", "--", *command],
                                    cwd=ROOT, capture_output=True, text=True,
                                    env=os.environ | {"RUSTC_WRAPPER": str(client), "TEST_STATS": str(stats),
                                                      "TEST_CALLS": str(calls), "TMPDIR": str(root)})
            self.assertEqual(result.returncode, 7, result.stdout + result.stderr)
            self.assertEqual(calls.read_text().splitlines(), ["read", "read"])
            marker = next(line.strip().split("=", 1)[1] for line in result.stdout.splitlines()
                          if "compiler_cache=" in line)
            self.assertEqual(json.loads(marker), {"observed": True, "cache_hits": 17, "cache_misses": 4})
            self.assertNotIn("secret-token", result.stdout + result.stderr)
            self.assertNotIn("private-endpoint", result.stdout + result.stderr)

    def test_cache_reset_or_missing_metrics_are_unknown_not_zero(self):
        self.assertEqual(metrics.delta({"observed": True, "cache_hits": 5, "cache_misses": 2},
                                       {"observed": True, "cache_hits": 1, "cache_misses": 1}),
                         {"observed": False, "reason": "counter_reset"})
        self.assertFalse(metrics.delta({"observed": False}, {"observed": True})["observed"])
        self.assertFalse(metrics.delta({"observed": True}, {"observed": True})["observed"])
        self.assertEqual(metrics.count({"counts": {}}), 0)
        self.assertIsNone(metrics.count({"counts": {"Rust": True}}))
        self.assertEqual(metrics.duration({"secs": 1, "nanos": 500000000}), 1.5)
        self.assertIsNone(metrics.duration({"secs": 1, "nanos": 1000000000}))
        self.assertIsNone(metrics.duration(None))
        report = metrics.delta({"observed": True, "cache_hits": 5, "cache_misses": 2,
                                "cache_read_hit_seconds": 1.5},
                               {"observed": True, "cache_hits": 7, "cache_misses": 3,
                                "cache_read_hit_seconds": 1.6})
        self.assertEqual(report["cache_read_hit_seconds"], 0.1)

    def test_hanging_stats_reader_is_bounded_and_private(self):
        with tempfile.TemporaryDirectory() as directory:
            client = Path(directory) / "sccache"
            client.write_text("#!/usr/bin/env python3\nimport time\ntime.sleep(30)\n")
            client.chmod(0o755)
            started = time.monotonic()
            result = subprocess.run(["python3", str(HELPER), "snapshot"], capture_output=True,
                                    text=True, timeout=3, env=os.environ | {"RUSTC_WRAPPER": str(client)})
            self.assertLess(time.monotonic() - started, 2)
            self.assertEqual(json.loads(result.stdout), {"observed": False, "reason": "unavailable"})

    def test_oversized_or_failed_client_does_not_publish_its_output(self):
        with tempfile.TemporaryDirectory() as directory:
            client = Path(directory) / "sccache"
            for body in ["print('secret-'*20000)", "import sys;print('secret',file=sys.stderr);sys.exit(7)"]:
                client.write_text("#!/usr/bin/env python3\n" + body + "\n")
                client.chmod(0o755)
                result = subprocess.run(["python3", str(HELPER), "snapshot"], capture_output=True,
                                        text=True, timeout=3, env=os.environ | {"RUSTC_WRAPPER": str(client)})
                self.assertEqual(json.loads(result.stdout), {"observed": False, "reason": "unavailable"})
                self.assertNotIn("secret", result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
