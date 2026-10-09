"""Tests of the performance record format (tools/perf/record.py)."""

from __future__ import annotations

import json
import os
import random
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parents[1] / "scripts" / "perf"))

import record  # noqa: E402
import runtime_benchmark  # noqa: E402

ENV = {"commit": "a" * 40, "profile": "dev", "renderer": "none", "virtualization": "kvm",
       "runner": "runner", "hardware": "test cpu; 4 cpus", "started_at": "2026-10-09T01:15:00Z"}


class Percentiles(unittest.TestCase):
    def test_nearest_rank_matches_runtime_benchmark(self):
        generator = random.Random(580)
        for size in (1, 2, 3, 7, 50, 99, 100, 101, 1000):
            samples = [generator.uniform(0.1, 50.0) for _ in range(size)]
            with self.subTest(size=size):
                expected = runtime_benchmark.distribution(samples, scale=1)
                got = record.from_samples("render.frame_ms.4k", "ms", samples, **ENV)
                self.assertEqual(got.n, expected["n"])
                self.assertEqual(got.p50, expected["p50_ms"])
                self.assertEqual(got.p95, expected["p95_ms"])
                self.assertEqual(got.p99, expected["p99_ms"])
                self.assertEqual(got.max, expected["max_ms"])

    def test_empty_samples_are_refused(self):
        with self.assertRaises(ValueError):
            record.from_samples("render.frame_ms.4k", "ms", [], **ENV)


class Validation(unittest.TestCase):
    def test_environment_labels_are_closed_sets(self):
        for name, value in (("renderer", "software"), ("virtualization", "vm"), ("profile", "debug")):
            with self.subTest(name=name), self.assertRaises(ValueError):
                record.from_samples("render.frame_ms.4k", "ms", [1.0], **{**ENV, name: value})

    def test_missing_environment_is_refused(self):
        partial = {name: value for name, value in ENV.items() if name != "commit"}
        with self.assertRaises(ValueError):
            record.from_samples("render.frame_ms.4k", "ms", [1.0], **partial)

    def test_kpi_ids_and_units_are_checked(self):
        with self.assertRaises(ValueError):
            record.from_samples("Frame Time", "ms", [1.0], **ENV)
        with self.assertRaises(ValueError):
            record.from_samples("render.frame_ms.4k", "seconds", [1.0], **ENV)

    def test_round_trip_and_unknown_fields(self):
        item = record.from_samples("agent.invoke.local.noop", "ms", [1, 2, 3], meta={"runs": "3"}, **ENV)
        again = record.parse_lines(item.to_json())[0]
        self.assertEqual(again, item)
        data = json.loads(item.to_json())
        data["secret"] = "x"
        with self.assertRaises(ValueError):
            record.parse_lines(json.dumps(data))

    def test_env_key_leaves_out_hardware_and_commit(self):
        one = record.from_samples("fs.x-y", "ms", [1.0], **ENV)
        other = record.from_samples("fs.x-y", "ms", [1.0], **{**ENV, "hardware": "other", "commit": "b" * 40})
        self.assertEqual(one.env_key, other.env_key)
        software = record.from_samples("fs.x-y", "ms", [1.0], **{**ENV, "renderer": "llvmpipe"})
        self.assertNotEqual(one.env_key, software.env_key)


class CommandLine(unittest.TestCase):
    def test_emit_writes_one_line(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "samples.txt").write_text("3\n1\n2\n")
            (root / "env.json").write_text(json.dumps(ENV))
            out = root / "out.txt"
            with out.open("w") as stream:
                old, sys.stdout = sys.stdout, stream
                try:
                    status = record.main(["emit", "--kpi", "fs.a-b", "--unit", "ms", "--samples-file",
                                          str(root / "samples.txt"), "--env-json", str(root / "env.json"),
                                          "--meta", "runs=3"])
                finally:
                    sys.stdout = old
            self.assertEqual(status, 0)
            [item] = record.read(out)
            self.assertEqual((item.n, item.p50, item.max, item.meta), (3, 2.0, 3.0, {"runs": "3"}))

    def test_env_detects_the_rest(self):
        with mock.patch.dict(os.environ, {"GITHUB_SHA": "c" * 40}):
            env = record.environment("release", "none", "none", "none", hardware="fixture")
        self.assertEqual(env["commit"], "c" * 40)
        self.assertEqual(env["hardware"], "fixture")
        self.assertRegex(env["started_at"], r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$")


if __name__ == "__main__":
    unittest.main()
