"""Actual cgroup reader and SDK diagnostic preserve coverage and fail closed."""
from __future__ import annotations

import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
HELPER = ROOT / "tools/ci/qualify_native_envelope.py"


def job(source: str, name: str) -> str:
    start = re.search(rf"(?m)^  {re.escape(name)}:\n", source)
    if start is None:
        raise AssertionError("cannot observe job " + name)
    remaining = source[start.end():]
    end = re.search(r"(?m)^  (?:[a-z][a-z-]*:\n|#)", remaining)
    return remaining[:end.start()] if end else remaining


def steps(body: str) -> dict[str, str]:
    matches = list(re.finditer(r"(?m)^      - name: (.+)\n", body))
    if not matches:
        raise AssertionError("cannot observe named steps")
    return {match.group(1): body[match.start():matches[i + 1].start() if i + 1 < len(matches) else len(body)].rstrip()
            for i, match in enumerate(matches)}


class NativeEnvelope(unittest.TestCase):
    def fixture(self, directory: Path) -> dict:
        for name, value in {"memory.max": str(22 * 1024**3), "memory.current": "4096",
                            "memory.peak": "8192", "memory.events": "max 0\noom 0\noom_kill 0\n",
                            "cpu.max": "2400000 100000"}.items():
            (directory / name).write_text(value + "\n")
        return {**os.environ, "TILO_CGROUP_MEMORY_PATH": str(directory)}

    def invoke(self, phase: str, directory: Path, env: dict):
        return subprocess.run([sys.executable, str(HELPER), phase, "--memory-gib", "22",
                               "--cpus", "24", "--state", str(directory / "before.json")],
                              env=env, capture_output=True, text=True)

    def test_actual_reader_accepts_matching_enforced_envelope(self):
        with tempfile.TemporaryDirectory() as value:
            directory = Path(value)
            env = self.fixture(directory)
            self.assertEqual(self.invoke("before", directory, env).returncode, 0)
            result = self.invoke("after", directory, env)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn('"event_deltas": {"max": 0, "oom": 0, "oom_kill": 0}', result.stdout)

    def test_absent_or_different_subject_cannot_pass(self):
        cases = {"memory.max": [None, "max", str(36 * 1024**3)],
                 "cpu.max": [None, "max 100000", "600000 100000", "0 0"],
                 "memory.peak": [None, "unavailable"],
                 "memory.events": [None, "oom 0\noom_kill 0"]}
        for filename, values in cases.items():
            for content in values:
                with self.subTest(filename=filename, content=content), tempfile.TemporaryDirectory() as value:
                    directory = Path(value)
                    env = self.fixture(directory)
                    if content is None:
                        (directory / filename).unlink()
                    else:
                        (directory / filename).write_text(content + "\n")
                    self.assertNotEqual(self.invoke("before", directory, env).returncode, 0)

    def test_any_memory_pressure_or_counter_reset_fails_after(self):
        for counters in ["max 1\noom 0\noom_kill 0", "max 0\noom 1\noom_kill 0",
                         "max 0\noom 0\noom_kill 1", "max 0\noom 0\noom_kill 0"]:
            with self.subTest(counters=counters), tempfile.TemporaryDirectory() as value:
                directory = Path(value)
                env = self.fixture(directory)
                if counters.endswith("oom_kill 0") and counters.startswith("max 0\noom 0"):
                    (directory / "memory.events").write_text("max 1\noom 0\noom_kill 0\n")
                self.assertEqual(self.invoke("before", directory, env).returncode, 0)
                (directory / "memory.events").write_text(counters + "\n")
                self.assertNotEqual(self.invoke("after", directory, env).returncode, 0)

    def test_peak_reaching_the_limit_fails(self):
        with tempfile.TemporaryDirectory() as value:
            directory = Path(value)
            env = self.fixture(directory)
            self.assertEqual(self.invoke("before", directory, env).returncode, 0)
            (directory / "memory.peak").write_text(str(22 * 1024**3))
            self.assertNotEqual(self.invoke("after", directory, env).returncode, 0)

    def test_changed_cgroup_cannot_reuse_before_evidence(self):
        with tempfile.TemporaryDirectory() as first, tempfile.TemporaryDirectory() as second:
            directory = Path(first)
            env = self.fixture(directory)
            self.assertEqual(self.invoke("before", directory, env).returncode, 0)
            replacement = self.fixture(Path(second))
            result = self.invoke("after", directory, replacement)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("cgroup changed", result.stderr)

    def test_diagnostic_executes_every_production_sdk_step_on_fixed_source(self):
        production = (ROOT / ".forgejo/workflows/app-sdk.yml").read_text()
        diagnostic = (ROOT / ".forgejo/workflows/sdk-native-envelope.yml").read_text()
        self.assertIn("  workflow_dispatch:\n", diagnostic)
        self.assertNotRegex(diagnostic, r"(?m)^  (push|pull_request):")
        original = steps(job(production, "linux"))
        measured = steps(job(diagnostic, "sdk"))
        self.assertNotIn("Merge onto main", measured)
        for name, body in original.items():
            if name in {"Merge onto main", "Verify native Linux SDK runner architecture"}:
                continue
            self.assertEqual(measured[name], body, name)
        self.assertIn("EXPECTED_ARCH: aarch64", measured["Verify native Linux SDK runner architecture"])
        self.assertIn('bash os/dev/assert-native-arch.sh "$EXPECTED_ARCH"', measured["Verify native Linux SDK runner architecture"])
        self.assertIn("options: --memory=${{ needs.builder.outputs.memory }}g --cpus=24", job(diagnostic, "sdk"))
        self.assertIn("runs-on: runner-large-arm64", job(diagnostic, "sdk"))
        self.assertIn("22|36)", job(diagnostic, "builder"))
        self.assertIn("exit 2", job(diagnostic, "builder"))
        env = lambda text: re.search(r"(?ms)^env:\n(.*?)(?=^jobs:)", text).group(1).rstrip()
        self.assertEqual(env(production), env(diagnostic))


if __name__ == "__main__":
    unittest.main()
