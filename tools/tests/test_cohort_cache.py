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
import re
import textwrap

ROOT = Path(__file__).resolve().parents[2]
HELPER = ROOT / "tools/ci/profile_cohort.py"
spec = importlib.util.spec_from_file_location("cohort_profile", HELPER)
profile = importlib.util.module_from_spec(spec)
spec.loader.exec_module(profile)

class NativeQualificationTests(unittest.TestCase):
    def test_dispatch_cannot_succeed_with_skipped_native_jobs(self):
        workflow = (ROOT / ".forgejo/workflows/cohort-cache-qualification.yml").read_text()
        aggregate = workflow.split("  qualification:\n", 1)[1]
        command = aggregate.split("python3 - <<'PY'\n", 1)[1].split("          PY\n", 1)[0]
        with tempfile.TemporaryDirectory() as directory:
            event = Path(directory) / "event.json"
            for name, inputs, builder, producer, passed in (
                    ("workflow_dispatch", {"components": "apps+runtime"}, "skipped", "skipped", False),
                    ("workflow_dispatch", {"components": "apps+runtime"}, "success", "success", True),
                    ("pull_request", {}, "skipped", "skipped", True),
                    ("", {}, "skipped", "skipped", False)):
                event.write_text(json.dumps({"inputs": inputs}))
                env = {**os.environ, "GITHUB_EVENT_PATH": str(event), "EVENT": name,
                       "TEST_RESULT": "success", "BUILDER_RESULT": builder, "PRODUCER_RESULT": producer}
                result = subprocess.run([sys.executable, "-c", textwrap.dedent(command)],
                                        env=env, capture_output=True, text=True)
                with self.subTest(event=name, producer=producer):
                    self.assertEqual(result.returncode == 0, passed, result.stderr)

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
        workflow = (ROOT / ".forgejo/workflows/cohort-cache-qualification.yml").read_text()
        sections = dict(re.findall(r"^  ([\w-]+):\n(.*?)(?=^  [\w-]+:\n|\Z)",
                                   workflow.split("jobs:\n", 1)[1], re.M | re.S))
        self.assertEqual(set(sections), {"tests", "builder", "producer", "qualification"})
        for name in ("producer", "builder"):
            self.assertIn("    if: ${{ github.event_name == 'workflow_dispatch' }}\n", sections[name])
        blocks = re.findall(r"^        run: \|\n((?:          .*\n|\n)*)", workflow, re.M)
        self.assertEqual(len(blocks), workflow.count("        run: |"))
        commands = "\n".join(blocks)
        for forbidden in ("virsh", "libvirt", "qemu", "build-image.sh", "tilo-vm vm", "tilo-vm swap",
                          "scripts/vm-acceptance/run.py", "swap-artifact", "remote_builders", "sudo"):
            self.assertNotIn(forbidden, commands)
        self.assertIn("    if: ${{ always() }}", sections["qualification"])
        self.assertIn("requested native producer did not qualify", sections["qualification"])
        self.assertIn("GITHUB_EVENT_PATH", sections["qualification"])
        calls = re.findall(r"^\s+(?:- )?uses: (\S+)", workflow, re.M)
        allowed = {
            "actions/checkout@11d5960a326750d5838078e36cf38b85af677262",
            "actions/upload-artifact@16871d9e8cfcf27ff31822cac382bbb5450f1e1e",
            "actions/download-artifact@d8d0a99033603453ad2255e58720b460a0555e1e",
            "./.forgejo/workflows/resolve-builder.yml",
        }
        self.assertTrue(calls)
        self.assertTrue(set(calls) <= allowed)
        self.assertIn('test "$(git rev-parse HEAD)" = "$SOURCE"', commands)
        self.assertIn('git status --porcelain --untracked-files=no', commands)
        self.assertIn('--repo "$PWD"', commands)
        self.assertIn('layer_artifacts.py verify', commands)
        self.assertIn('--receipt-sha256 "$RECEIPT_SHA256"', commands)
        self.assertIn('--run-id "$GITHUB_RUN_ID" --run-attempt "$GITHUB_RUN_ATTEMPT"', commands)
        self.assertIn('--check-telemetry', commands)
        transfer = "name: native-cohort-${{ github.run_id }}-${{ github.run_attempt }}-${{ github.sha }}"
        self.assertEqual(workflow.count(transfer), 2)


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
