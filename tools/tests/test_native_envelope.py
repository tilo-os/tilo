"""Actual cgroup reader and SDK diagnostic preserve coverage and fail closed."""
from __future__ import annotations

import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools" / "ci"))
import t_matrix  # noqa: E402
HELPER = ROOT / "tools/ci/qualify_native_envelope.py"


class NativeEnvelope(unittest.TestCase):
    def fixture(self, directory: Path) -> dict:
        for name, value in {"memory.max": str(22 * 1024**3), "memory.current": "4096",
                            "memory.peak": "8192", "memory.events": "max 0\noom 0\noom_kill 0\n",
                            "cpu.max": "2400000 100000", "memory.swap.max": "0",
                            "memory.swap.current": "0"}.items():
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
                 "memory.swap.max": [None, "max", "1"],
                 "memory.swap.current": [None, "1"],
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

    def test_prepare_only_lowers_own_swap_without_resetting_other_resources(self):
        with tempfile.TemporaryDirectory() as value:
            directory = Path(value)
            env = self.fixture(directory)
            (directory / "memory.swap.max").write_text("max\n")
            previous = {p.name: p.read_bytes() for p in directory.iterdir()}
            result = self.invoke("prepare", directory, env)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual((directory / "memory.swap.max").read_text(), "0\n")
            for name, data in previous.items():
                if name != "memory.swap.max":
                    self.assertEqual((directory / name).read_bytes(), data, name)
            self.assertEqual(self.invoke("before", directory, env).returncode, 0)

    def test_prepare_refuses_wrong_job_caps_or_existing_swap_without_writing(self):
        for name, value in [("memory.max", str(36 * 1024**3)), ("cpu.max", "600000 100000"),
                            ("memory.swap.current", "1")]:
            with self.subTest(name=name), tempfile.TemporaryDirectory() as temporary:
                directory = Path(temporary)
                env = self.fixture(directory)
                (directory / "memory.swap.max").write_text("max\n")
                (directory / name).write_text(value + "\n")
                self.assertNotEqual(self.invoke("prepare", directory, env).returncode, 0)
                self.assertEqual((directory / "memory.swap.max").read_text(), "max\n")

    def test_t_checks_build_on_arm64_inside_the_qualified_envelope(self):
        checks = {check["name"]: check for check in t_matrix.load(ROOT)["check"]}
        wrapper = (ROOT / "tools/ci/native-envelope.sh").read_text()
        self.assertIn('--memory "${memory}g" --memory-swap "${memory}g" --cpus 24', wrapper)
        for phase in ("envelope prepare", "envelope before", "envelope after || status=1"):
            self.assertIn(phase, wrapper)
        self.assertIn("22|36) ;;", wrapper)
        for name, script in (("sdk-envelope", "tools/ci/envelope/sdk.sh"),
                             ("web-size-envelope-bundle", "tools/ci/envelope/web-size.sh")):
            with self.subTest(check=name):
                check = checks[name]
                self.assertEqual(check["requires"], ["arch=arm64"])
                self.assertTrue(check["privileged"])
                self.assertFalse(check["cache"])
                self.assertEqual(check["dispatch_inputs"], {"memory_gib": "22"})
                self.assertIn("tools/ci/native-envelope.sh", check["run"])
                self.assertIn(script, check["run"])
        self.assertEqual(checks["sdk-envelope"]["on"], ["dispatch"])
        browser = checks["web-size-envelope"]
        self.assertEqual(browser["on"], ["dispatch"])
        self.assertEqual(browser["needs"], ["web-size-envelope-bundle"])
        self.assertEqual(browser["requires"], ["arch=x86_64"])
        self.assertIn("release_compute.py verify envelope/release-compute", browser["run"])
        self.assertIn("qualify-packaged.mjs envelope/release-compute", browser["run"])
        sdk = (ROOT / "tools/ci/envelope/sdk.sh").read_text()
        for command in ("cargo test --locked -p tilo-caps", "cargo test --locked -p tilo-app-cli --lib targets::",
                        "--test it outside_repo -- --ignored", "package --target linux apps/calculator"):
            self.assertIn(command, sdk)
            self.assertIn(command, checks["sdk-linux"]["run"])

if __name__ == "__main__":
    unittest.main()
