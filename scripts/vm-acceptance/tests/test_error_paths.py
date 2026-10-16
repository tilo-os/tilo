"""Acceptance checks must fail when their evidence or workload is unavailable."""

import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

HERE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HERE))
from acceptance import core
from acceptance.checks import provenance, resources
from acceptance.vm import VmError

spec = importlib.util.spec_from_file_location("acceptance_runner", HERE / "run.py")
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


class ErrorPaths(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.out = Path(self.tmp.name)
        self.ctx = SimpleNamespace(out=self.out, facts={}, config={}, vm=Mock())

    def run_check(self, func):
        chk = next(c for c in core.REGISTRY if c.func is func)
        return core.run_check(self.ctx, chk)

    def provenance_result(self, status, record=None):
        disk = self.out / "disk.qcow2"
        self.record_path = Path(str(disk) + ".provenance.json")
        self.record_path.write_text(json.dumps(record if record is not None else {"commit": "a" * 40}))
        if isinstance(status, Exception):
            self.ctx.vm.vm_cmd.side_effect = status
        else:
            self.ctx.vm.vm_cmd.return_value = json.dumps(status)
        self.ctx.facts["provenance"] = runner.provenance_of(self.ctx.vm, disk, "a" * 40)
        return self.run_check(provenance.commit)

    def test_bootc_failure_does_not_pass_with_disk_record(self):
        result = self.provenance_result(VmError("bootc unavailable"))
        self.assertEqual(result.status, "fail")
        self.assertTrue(result.unobservable)
        self.assertIn("bootc unavailable", result.detail)
        self.assertNotIn("booted_digest", json.loads(self.record_path.read_text()))

    def test_missing_booted_digest_is_not_pinned_or_accepted(self):
        for status in ({}, {"status": {"booted": {"image": {}}}}):
            with self.subTest(status=status):
                result = self.provenance_result(status)
                self.assertEqual(result.status, "fail")
                self.assertIn("no booted image digest", result.detail)
                self.assertNotIn("booted_digest", json.loads(self.record_path.read_text()))

    def test_missing_record_commit_does_not_match_every_commit(self):
        result = self.provenance_result(self.booted(), {"subject": "image"})
        self.assertEqual(result.status, "fail")
        self.assertIn("no commit", result.detail)

    @staticmethod
    def booted():
        return {"status": {"booted": {"image": {"imageDigest": "sha256:abc"}}}}

    def test_valid_first_boot_pins_digest_and_passes(self):
        result = self.provenance_result(self.booted())
        self.assertEqual(result.status, "pass", result.detail)
        self.assertEqual(json.loads(self.record_path.read_text())["booted_digest"], "sha256:abc")

    def test_pretty_printed_bootc_status_with_cli_receipt(self):
        self.provenance_result(self.booted())
        self.ctx.vm.vm_cmd.return_value = (
            "connecting to VM\n" + json.dumps(self.booted(), indent=2) + "\nreceipt: complete\n"
        )
        disk = self.out / "disk.qcow2"
        self.ctx.facts["provenance"] = runner.provenance_of(self.ctx.vm, disk, "a" * 40)
        result = self.run_check(provenance.commit)
        self.assertEqual(result.status, "pass", result.detail)

    def test_restart_fallback_only_queries_services(self):
        self.service_tools()
        execute = self.ctx.vm.exec.side_effect

        def mixed_units(script, **kwargs):
            if "list-units --all" in script:
                return "R tilo-test.service 0\nR tilo-app-host.socket\nR tilo-graphical-session.target\n"
            return execute(script, **kwargs)

        self.ctx.vm.exec.side_effect = mixed_units
        result = self.run_check(provenance.services)
        self.assertEqual(result.status, "pass", result.detail)
        query = self.ctx.vm.exec.call_args.args[0]
        self.assertIn("list-units --type=service", query)

    def service_tools(self, failed=False, journal=False, listing=False, show=False, value="0"):
        # Execute the production POSIX scripts with fake commands, so masked pipeline
        # and command-substitution exit statuses are tested rather than mocked away.
        tools = self.out / "bin"
        tools.mkdir()
        systemctl = tools / "systemctl"
        systemctl.write_text(
            "#!/bin/sh\n"
            'case "$*" in\n'
            f"  *--failed*) {'echo bus-unavailable >&2; exit 7' if failed else 'exit 0'};;\n"
            f"  *list-units*) {'echo list-failed >&2; exit 8' if listing else 'echo tilo-test.service loaded active running'};;\n"
            f"  *show*) {'echo show-failed >&2; exit 9' if show else 'echo ' + value};;\n"
            "esac\n")
        journalctl = tools / "journalctl"
        journalctl.write_text("#!/bin/sh\n" + ("exit 0\n" if journal else "echo journal-unavailable >&2; exit 1\n"))
        systemctl.chmod(0o755)
        journalctl.chmod(0o755)

        def execute(script, **kwargs):
            env = dict(os.environ, PATH=str(tools) + os.pathsep + os.environ["PATH"])
            proc = subprocess.run(["sh", "-c", script], capture_output=True, text=True, env=env)
            if kwargs.get("check", True) and proc.returncode:
                raise VmError(proc.stderr.strip())
            return proc.stdout

        self.ctx.vm.exec.side_effect = execute

    def test_failed_unit_query_cannot_look_healthy(self):
        self.service_tools(failed=True, journal=True)
        result = self.run_check(provenance.services)
        self.assertEqual(result.status, "fail")
        self.assertIn("bus-unavailable", result.detail)

    def test_fallback_listing_failure_cannot_look_empty(self):
        self.service_tools(listing=True)
        result = self.run_check(provenance.services)
        self.assertEqual(result.status, "fail")
        self.assertIn("list-failed", result.detail)

    def test_fallback_show_failure_preserves_cause(self):
        self.service_tools(show=True)
        result = self.run_check(provenance.services)
        self.assertEqual(result.status, "fail")
        self.assertIn("show-failed", result.detail)

    def test_malformed_fallback_counter_is_not_ignored(self):
        self.service_tools(value="unknown")
        result = self.run_check(provenance.services)
        self.assertEqual(result.status, "fail")
        self.assertIn("invalid NRestarts", result.detail)

    def test_readable_empty_journal_is_healthy(self):
        self.service_tools(journal=True)
        result = self.run_check(provenance.services)
        self.assertEqual(result.status, "pass", result.detail)
        self.assertEqual(result.metrics["restart_source"], "journalctl --user -b")

    def test_journal_unavailable_uses_successful_fallback(self):
        self.service_tools()
        result = self.run_check(provenance.services)
        self.assertEqual(result.status, "pass", result.detail)
        self.assertIn("NRestarts", result.metrics["restart_source"])
        self.assertIn("journal-unavailable", result.metrics["journal_error"])

    def test_fallback_still_detects_restart_loop(self):
        self.service_tools(value="4")
        result = self.run_check(provenance.services)
        self.assertEqual(result.status, "fail")
        self.assertIn("restart loops", result.detail)

    def test_memory_cannot_pass_after_failed_app_launches(self):
        self.ctx.reset = Mock()
        self.ctx.open_app = Mock(side_effect=core.CheckFailed("first frame never arrived"))
        sample = self.configure_memory()
        with patch.object(resources.time, "sleep"), patch.object(resources, "memory", return_value=sample) as memory:
            result = self.run_check(resources.mem)
        self.assertEqual(result.status, "fail")
        self.assertTrue(result.unobservable)
        self.assertIn("first frame never arrived", result.detail)
        self.assertEqual(self.ctx.open_app.call_count, 4)
        memory.assert_not_called()

    def test_partial_workload_fact_does_not_bypass_launches(self):
        self.ctx.facts["apps_open_for_memory"] = [resources.MEMORY_APPS[0]]
        self.ctx.reset = Mock()
        self.ctx.open_app = Mock(side_effect=core.CheckFailed("launch failed"))
        result = self.run_check(resources.mem)
        self.assertEqual(result.status, "fail")
        self.assertIn("launch failed", result.detail)
        self.ctx.reset.assert_called_once()

    def configure_memory(self):
        self.ctx.write = Mock()
        self.ctx.config["budgets"] = {"shell_pss_mb": 700, "process_pss_mb": 300, "session_pss_mb": 2500}
        return {"procs": [{"comm": "shell", "pss_mb": 20}], "pss_available": True,
                "pss_mb_total": 20, "rss_mb_total": 30, "anon_rss_mb_total": 10, "guest_mem_mb": {}}

    def test_complete_memory_workload_can_pass(self):
        self.ctx.reset = Mock()
        self.ctx.open_app = Mock()
        sample = self.configure_memory()
        with patch.object(resources.time, "sleep"), patch.object(resources, "memory", return_value=sample):
            result = self.run_check(resources.mem)
        self.assertEqual(result.status, "pass", result.detail)
        self.assertEqual(self.ctx.open_app.call_count, 4)
        self.assertEqual(self.ctx.facts["apps_open_for_memory"], list(resources.MEMORY_APPS))


if __name__ == "__main__":
    unittest.main()
