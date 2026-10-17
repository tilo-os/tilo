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
        self.ctx = SimpleNamespace(out=self.out, facts={}, config={}, vm=Mock(),
                                   state=Mock(return_value={"chrome": {}}), shot=Mock(),
                                   write=Mock(), record=Mock())
        self.ctx.vm.slot = "acceptance"
        self.ctx.vm.requested_restarts = 0
        self.ctx.vm.root_exec.return_value = "NRestarts=0\n"
        self.addCleanup(patch.stopall)
        patch.object(core, "terminal_shells", return_value=set()).start()
        patch.object(core, "wait_new_terminal_shell", return_value=0).start()

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
        self.ctx.facts["apps_open_for_memory"] = [resources.FOUR_APPS[0]]
        self.ctx.reset = Mock()
        self.ctx.open_app = Mock(side_effect=core.CheckFailed("launch failed"))
        with patch.object(resources.time, "sleep"):
            result = self.run_check(resources.mem)
        self.assertEqual(result.status, "fail")
        self.assertIn("launch failed", result.detail)
        self.ctx.reset.assert_called_once()

    def test_closed_app_cannot_be_measured_as_a_complete_workload(self):
        self.ctx.reset = Mock()
        self.ctx.open_app = Mock(side_effect=[({"id": i}, {}) for i in range(4)])
        self.ctx.vm.exec.return_value = "app exited\n"
        with (patch.object(resources.time, "sleep"),
              patch.object(core, "group_by_id", side_effect=[None, {"id": 1}, {"id": 2}, {"id": 3}]),
              patch.object(resources, "memory") as memory):
            result = self.run_check(resources.mem)
        self.assertEqual(result.status, "fail")
        self.assertTrue(result.unobservable)
        self.assertIn("closed on its own", result.detail)
        self.assertEqual(self.ctx.facts["apps_closed_after_open"], [resources.FOUR_APPS[0]])
        memory.assert_not_called()
        self.ctx.write.assert_called_once()

    def test_custom_workload_keeps_the_current_opener_api(self):
        self.ctx.reset = Mock()
        self.ctx.open_app = Mock(return_value=({"id": 1}, {}))
        apps = ("so.tilo.notes",)
        with (patch.object(resources.time, "sleep"),
              patch.object(core, "group_by_id", return_value={"id": 1})):
            opened = resources._open_several(self.ctx, apps, "R09")
        self.assertEqual(opened, list(apps))
        self.ctx.open_app.assert_called_once_with(apps[0])

    def test_partial_relay_setup_removes_the_already_certified_peer(self):
        device = "ab" * 32
        self.ctx.vm.exec.side_effect = [json.dumps({"device_public_key": device,
                                                  "relay_service_node_id": "cd" * 32}),
                                        "Failed to start transient unit"]
        self.ctx.vm.slot_uid.return_value = 1001
        with patch.object(resources, "stop_home_relay") as stop:
            with self.assertRaisesRegex(core.Unobservable, "did not start"):
                resources.start_home_relay(self.ctx)
        stop.assert_called_once_with(self.ctx, device)

    def test_partial_setup_preserves_original_failure_when_cleanup_also_fails(self):
        self.ctx.vm.exec.side_effect = [json.dumps({"device_public_key": "ab" * 32,
                                                  "relay_service_node_id": "cd" * 32}),
                                        VmError("setup connection lost")]
        self.ctx.vm.slot_uid.return_value = 1001
        with patch.object(resources, "stop_home_relay", side_effect=VmError("stop refused")):
            with self.assertRaisesRegex(VmError, "setup connection lost") as raised:
                resources.start_home_relay(self.ctx)
        self.assertIn("stop refused", " ".join(raised.exception.__notes__))

    def relay_tools(self, stop=False, rewrite=False):
        tools = self.out / "bin"
        tools.mkdir()
        for name, body in {
                "journalctl": "exit 1",  # Optional journal failure must not prevent cleanup.
                "systemctl": ('case "$*" in *show*) echo loaded;; *stop*) echo stop-refused >&2; exit 7;; esac'
                              if stop else 'echo not-found'),
                "mv": "echo rewrite-refused >&2; exit 8" if rewrite else 'exec /usr/bin/mv "$@"',
        }.items():
            path = tools / name
            path.write_text("#!/bin/sh\n" + body + "\n")
            path.chmod(0o755)
        identity = self.out / "identity"
        (identity / "peers").mkdir(parents=True)
        runtime = self.out / "runtime"
        runtime.mkdir()
        relay = runtime / resources.HOME_RELAY_DIR
        relay.mkdir()
        (relay / "state").write_text("keep if stop fails")
        env = dict(os.environ, PATH=str(tools) + os.pathsep + os.environ["PATH"],
                   XDG_RUNTIME_DIR=str(runtime), TILO_IDENTITY_ROOT=str(identity))

        def execute(script, **kwargs):
            proc = subprocess.run(["sh", "-c", script], capture_output=True, text=True, env=env)
            if kwargs.get("check", True) and proc.returncode:
                raise VmError(proc.stderr.strip())
            return proc.stdout

        self.ctx.vm.exec.side_effect = execute
        return relay, identity / "peers" / "home-peers.json"

    def test_relay_setup_cannot_delete_state_when_previous_relay_refuses_stop(self):
        relay, _ = self.relay_tools(stop=True)
        with self.assertRaisesRegex(core.Unobservable, "stop-refused"):
            resources.start_home_relay(self.ctx)
        self.assertEqual((relay / "state").read_text(), "keep if stop fails")

    def test_relay_stop_failure_is_not_hidden_by_rm(self):
        relay, _ = self.relay_tools(stop=True)
        with self.assertRaisesRegex(VmError, "stop-refused"):
            resources.stop_home_relay(self.ctx, None)
        self.assertTrue((relay / "state").exists())

    def test_relay_peer_rewrite_failure_is_not_accepted(self):
        relay, peers = self.relay_tools(rewrite=True)
        device = "ab" * 32
        original = json.dumps([{"device_node_id": device}])
        peers.write_text(original)
        with self.assertRaisesRegex(VmError, "rewrite-refused"):
            resources.stop_home_relay(self.ctx, device)
        self.assertEqual(peers.read_text(), original)
        self.assertFalse(relay.exists())

    def test_absent_relay_and_unavailable_journal_still_allow_cleanup(self):
        relay, peers = self.relay_tools()
        device = "ab" * 32
        other = {"device_node_id": "cd" * 32}
        peers.write_text(json.dumps([{"device_node_id": device}, other]))
        resources.stop_home_relay(self.ctx, device)
        self.assertFalse(relay.exists())
        self.assertEqual(json.loads(peers.read_text()), [other])

    def test_shared_cleanup_cannot_pass_but_does_not_mask_original_failure(self):
        self.ctx.reset = Mock(side_effect=VmError("reset refused"))
        for original in (None, "measurement failed"):
            with self.subTest(original=original):
                def check(ctx, res):
                    try:
                        if original:
                            raise core.CheckFailed(original)
                    finally:
                        resources._leave_shared(ctx, res, 1, "ab" * 32)

                with (patch.object(resources, "_select_workspace", side_effect=VmError("select refused")) as select,
                      patch.object(resources, "stop_home_relay", side_effect=VmError("stop refused")) as stop):
                    result = core.run_check(self.ctx, core.Check("R09", "resources", "cleanup", check))
                self.assertEqual(result.status, "fail", result.detail)
                self.assertIn(original or "shared workspace cleanup failed", result.detail)
                self.assertEqual(len(result.metrics["cleanup_errors"]), 3)
                select.assert_called_once()
                stop.assert_called_once()

    def configure_memory(self):
        self.ctx.write = Mock()
        self.ctx.config["budgets"] = {"shell_anon_mb": 700, "process_anon_mb": 300,
                                      "session_memory_mb": 2500, "app_memory_mb": 300, "app_anon_mb": 200}
        return {"procs": [{"comm": "shell", "anon_rss_mb": 10}],
                "shell": {"pid": 1, "anon_rss_mb": 10}, "session_memory_mb": 30,
                "apps": {app: {"pids": [i + 2], "shared_with": {}, "memory_mb": 5, "anon_mb": 2}
                         for i, app in enumerate(resources.FOUR_APPS)}, "guest_mem_mb": {}}

    def test_complete_memory_workload_can_pass(self):
        self.ctx.reset = Mock()
        self.ctx.open_app = Mock(side_effect=[({"id": i}, {}) for i in range(4)])
        sample = self.configure_memory()
        after = {"guest": {"used_mib": 30, **{f"{k}_mib": 1 for k in
                  ("anon", "shmem", "kernel", "reserve", "untracked")}},
                 "vm_only_mib": 0, "buckets": {}, "unattributed_mib": 0, "processes": []}
        with (patch.object(resources.time, "sleep"),
              patch.object(resources, "memory", return_value=sample) as memory,
              patch.object(core, "group_by_id", return_value={"id": 1}),
              patch.object(core, "shell_memory", return_value={"shm_by_name": {}}),
              patch.object(resources, "_footprint_after_close", return_value=after),
              patch.object(resources, "write_footprint"),
              patch.object(resources, "shell_memory_map", return_value={}),
              patch.object(resources, "record_shell_parts"),
              patch.object(resources, "record_disk", return_value={})):
            result = self.run_check(resources.mem)
        memory.assert_called_once_with(self.ctx, list(resources.FOUR_APPS))
        self.assertEqual(result.status, "pass", result.detail)
        self.assertEqual(self.ctx.open_app.call_count, 4)
        self.assertEqual(self.ctx.facts["apps_open_for_memory"], list(resources.FOUR_APPS))


if __name__ == "__main__":
    unittest.main()
