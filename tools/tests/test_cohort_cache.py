"""The real producer boundary is independent of VM lifecycle operations."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
import yaml

ROOT = Path(__file__).resolve().parents[2]
HELPER = ROOT / "tools/ci/profile_cohort.py"
spec = importlib.util.spec_from_file_location("cohort_profile", HELPER)
profile = importlib.util.module_from_spec(spec)
spec.loader.exec_module(profile)

class NativeQualificationTests(unittest.TestCase):
    def observation(self):
        return {
            "schema": "tilo.vm.cohort-cache-observation/v1", "producer_exit_code": 0,
            "before": {"available": True, "cache_hits": 3, "cache_misses": 1},
            "after": {"available": True, "cache_hits": 5, "cache_misses": 2},
            "delta": {"cache_hits": 2, "cache_misses": 1},
        }

    def qualify(self, value):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "observation.json"
            path.write_text(json.dumps(value))
            return profile.qualify_observation(path)

    def test_actual_monotonic_observation_qualifies(self):
        self.assertEqual(self.qualify(self.observation()), {"cache_hits": 2, "cache_misses": 1})

    def test_unobserved_reset_zero_or_failed_producer_never_qualifies(self):
        cases = []
        value = self.observation(); value["before"]["available"] = False; cases.append(value)
        value = self.observation(); value["after"]["cache_hits"] = 1; cases.append(value)
        value = self.observation(); value["delta"]["cache_hits"] = 20; cases.append(value)
        value = self.observation(); value["after"] = value["before"].copy()
        value["delta"] = {"cache_hits": 0, "cache_misses": 0}; cases.append(value)
        value = self.observation(); value["producer_exit_code"] = 1; cases.append(value)
        value = self.observation(); value["before"]["cache_hits"] = True; cases.append(value)
        value = self.observation(); value["delta"]["cache_misses"] = True; cases.append(value)
        for value in cases:
            with self.subTest(value=value), self.assertRaises(ValueError):
                self.qualify(value)

    def test_qualification_cli_checks_report_without_starting_producer(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "observation.json"
            path.write_text(json.dumps(self.observation()))
            observed = subprocess.run([sys.executable, str(HELPER), "--telemetry", str(path),
                                       "--check-telemetry"], capture_output=True, text=True)
            self.assertEqual(observed.returncode, 0, observed.stderr)
            self.assertEqual(json.loads(observed.stdout), {"cache_hits": 2, "cache_misses": 1})

    def test_native_workflow_reaches_producer_and_verifier_without_vm_commands(self):
        workflow = yaml.load((ROOT / ".forgejo/workflows/cohort-cache-qualification.yml").read_text(),
                             Loader=yaml.BaseLoader)
        jobs = workflow["jobs"]
        self.assertEqual(jobs["producer"]["if"], "github.event_name == 'workflow_dispatch'")
        self.assertEqual(jobs["builder"]["if"], "github.event_name == 'workflow_dispatch'")
        commands = "\n".join(step.get("run", "") for job in jobs.values()
                             for step in job.get("steps", []))
        for forbidden in ("virsh", "libvirt", "qemu", "build-image.sh", "tilo-vm vm", "tilo-vm swap",
                          "scripts/vm-acceptance/run.py", "swap-artifact", "remote_builders", "sudo"):
            self.assertNotIn(forbidden, commands)
        self.assertIn('test "$(git rev-parse HEAD)" = "$SOURCE"', commands)
        self.assertIn('git status --porcelain --untracked-files=no', commands)
        self.assertIn('--repo "$PWD"', commands)
        self.assertIn('layer_artifacts.py verify', commands)
        self.assertIn('--receipt-sha256 "$RECEIPT_SHA256"', commands)
        self.assertIn('--run-id "$GITHUB_RUN_ID" --run-attempt "$GITHUB_RUN_ATTEMPT"', commands)
        self.assertIn('--check-telemetry', commands)
        artifacts = [step for step in jobs["producer"]["steps"] if "uses" in step]
        names = [step.get("with", {}).get("name") for step in artifacts if "artifact@" in step["uses"]]
        self.assertEqual(names[:2], ["native-cohort-${{ github.run_id }}-${{ github.run_attempt }}-${{ github.sha }}"] * 2)

class CacheObservationTests(unittest.TestCase):
    def test_signal_exit_keeps_shell_status_and_raw_observation(self):
        root = Path(__file__).resolve().parents[2] / "target/cohort-cache-tests"
        root.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=root) as directory:
            output = Path(directory) / "observation.json"
            with patch.object(profile, "cache_snapshot", return_value={"available": False}), \
                 patch.object(profile.subprocess, "run", return_value=subprocess.CompletedProcess([], -15)):
                self.assertEqual(profile.run_profile(output, ["produce"]), 143)
            self.assertEqual(json.loads(output.read_text())["producer_exit_code"], -15)

    def test_totals_keep_rust_and_native_compilers(self):
        value = {"stats": {"cache_hits": {"counts": {"Rust": 8, "C/C++": 2}},
                           "cache_misses": {"counts": {"Rust": 3}}}}
        self.assertEqual(profile.totals(value), {"cache_hits": 10, "cache_misses": 3})
        value["stats"]["cache_hits"]["counts"]["Rust"] = True
        with self.assertRaises(ValueError):
            profile.totals(value)

    def test_unavailable_or_reset_counters_are_unknown(self):
        ready = {"available": True, "cache_hits": 10, "cache_misses": 3}
        self.assertIsNone(profile.delta({"available": False}, ready))
        self.assertIsNone(profile.delta(ready, {**ready, "cache_hits": 9}))
        self.assertEqual(profile.delta(ready, {**ready, "cache_hits": 12}),
                         {"cache_hits": 2, "cache_misses": 0})

    def test_failed_stats_do_not_publish_raw_backend_output(self):
        response = subprocess.CompletedProcess([], 1, b"private backend token", b"private error")
        with patch.dict(os.environ, {"RUSTC_WRAPPER": "/ci-tools/sccache"}, clear=True), \
             patch.object(profile.subprocess, "run", return_value=response):
            observed = profile.cache_snapshot()
        self.assertFalse(observed["available"])
        self.assertNotIn("private", json.dumps(observed))

    def test_real_producer_stdout_and_exit_survive_unavailable_cache(self):
        root = Path(__file__).resolve().parents[2] / "target/cohort-cache-tests"
        root.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=root) as directory:
            output = Path(directory) / "observation.json"
            producer = ROOT / "scripts/vm-acceptance/layer_artifacts.py"
            env = {**os.environ, "RUSTC_WRAPPER": "/nonexistent/sccache", "SCCACHE_GHA_VERSION": "tilo/apps/v2/x86_64"}
            env["TEST_PRIVATE_TOKEN"] = "must-not-be-recorded"
            for arguments in (["--help"], ["produce"]):
                original = subprocess.run([sys.executable, str(producer), *arguments],
                                          env=env, capture_output=True)
                observed = subprocess.run([sys.executable, profile.__file__, "--telemetry", str(output),
                                           "--", *arguments], env=env, capture_output=True)
                self.assertEqual(observed.returncode, original.returncode)
                self.assertEqual(observed.stdout, original.stdout)
                telemetry = json.loads(output.read_text())
                self.assertEqual(telemetry["producer_exit_code"], original.returncode)
                self.assertIsNone(telemetry["delta"])
                self.assertEqual(telemetry["configured_namespace"], "tilo/apps/v2/x86_64")
                self.assertNotIn("must-not-be-recorded", output.read_text())


if __name__ == "__main__":
    unittest.main()
