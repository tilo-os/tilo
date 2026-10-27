"""Canonical management bridge admission and readback regressions (no running VM)."""

import json
import sys
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import management
from acceptance.vm import VmError


class ManagementTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.source = Path(self.tmp.name)
        self.layer = "a" * 64
        self.receipt = self.source / "swap.json"
        self.record = {"status": "success", "vm": "fixture", "slot": "vm",
                       "component": "vmd", "layer": self.layer}
        self.receipt.write_text(json.dumps(self.record))
        self.base = Mock()
        self.current = Mock(vm="fixture")
        self.base.run.return_value = json.dumps({"receipt": str(self.receipt)})
        self.current.run.return_value = json.dumps({"slot": {"components": {"vmd": self.layer}}})

    def bridge(self):
        return management.bootstrap_management(self.base, self.current, self.source, "candidate")

    @patch("management.source_commit", return_value="b" * 40)
    def test_canonical_bridge_then_current_client_readback_without_session_restart(self, _head):
        result = self.bridge()
        self.base.run.assert_called_once_with("--json", "swap", "vmd", "--slot", "vm",
                                             "--builder", "host", timeout=5400, slot=False,
                                             cwd=self.source)
        self.current.run.assert_called_once_with("--json", "slot", "show", "vm",
                                                timeout=120, slot=False)
        self.assertEqual(result["commit"], "b" * 40)
        self.assertEqual(result["components"][0]["layer"], self.layer)

    @patch("management.source_commit", return_value="b" * 40)
    def test_failed_swap_aborts_before_new_client_or_session(self, _head):
        self.base.run.side_effect = VmError("admission refused")
        with self.assertRaisesRegex(VmError, "admission refused"):
            self.bridge()
        self.current.run.assert_not_called()

    @patch("management.source_commit", return_value="b" * 40)
    def test_current_protocol_refusal_remains_fatal(self, _head):
        self.current.run.side_effect = VmError("client 9, daemon 8")
        with self.assertRaisesRegex(VmError, "client 9, daemon 8"):
            self.bridge()

    @patch("management.source_commit", side_effect=["b" * 40, "c" * 40])
    def test_source_changed_aborts_before_readback(self, _head):
        with self.assertRaisesRegex(VmError, "source changed"):
            self.bridge()
        self.current.run.assert_not_called()

    @patch("management.source_commit", return_value="b" * 40)
    def test_foreign_receipt_and_layer_mismatch_refused(self, _head):
        for field, value in (("vm", "foreign"), ("component", "shell"), ("status", "failed"),
                             ("layer", "c" * 64)):
            with self.subTest(field=field):
                self.receipt.write_text(json.dumps({**self.record, field: value}))
                with self.assertRaises(VmError):
                    self.bridge()

    @patch("management.bootstrap_management")
    def test_equal_protocol_no_bootstrap_preserves_base(self, bridge):
        result = management.start_management(self.current, self.current, None, "base", None)
        self.assertIsNone(result)
        self.current.vm_cmd.assert_called_once_with("start", "fixture", timeout=600)
        bridge.assert_not_called()
        self.base.run.assert_not_called()

    @patch("management.bootstrap_management")
    def test_fixture_client_starts_before_bridge(self, bridge):
        events = []
        self.base.vm_cmd.side_effect = lambda *args, **kwargs: events.append(args[0])
        bridge.side_effect = lambda *args: events.append("bridge") or {"commit": "target"}
        result = management.start_management(self.current, self.base, self.source,
                                             "candidate", ["create", "fixture"])
        self.assertEqual(events, ["create", "start", "bridge"])
        self.assertEqual(result, {"commit": "target"})
        self.current.vm_cmd.assert_not_called()

    @patch("management.source_commit", return_value="b" * 40)
    def test_malformed_swap_response_fails_with_diagnostic(self, _head):
        self.base.run.return_value = "not JSON"
        with self.assertRaisesRegex(VmError, "invalid JSON"):
            self.bridge()
        self.current.run.assert_not_called()

    def test_real_git_protocol_planning_and_dirty_source_refusal(self):
        def git(*args):
            return subprocess.run(["git", "-C", str(self.source), *args], check=True,
                                  capture_output=True, text=True).stdout.strip()
        git("init")
        protocol = self.source / management.PROTOCOL_PATH
        protocol.parent.mkdir(parents=True)
        protocol.write_text("pub const PROTOCOL_VERSION: u32 = 8;\n")
        git("add", management.PROTOCOL_PATH)
        git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.test",
            "commit", "-m", "fixture eight")
        base = git("rev-parse", "HEAD")
        protocol.write_text("pub const PROTOCOL_VERSION: u32 = 9;\n")
        git("add", management.PROTOCOL_PATH)
        git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.test",
            "commit", "-m", "fixture nine")
        target = git("rev-parse", "HEAD")
        self.receipt.unlink()
        self.assertNotEqual(management.protocol_at(base, self.source),
                            management.protocol_at(target, self.source))
        self.assertEqual(management.source_commit(self.source), target)
        protocol.write_text("pub const PROTOCOL_VERSION: u32 = 10;\n")
        with self.assertRaisesRegex(VmError, "not a clean checkout"):
            management.source_commit(self.source)

    @patch("management.subprocess.run")
    def test_declared_equal_protocol_preserves_no_swap(self, command):
        command.return_value.stdout = "pub const PROTOCOL_VERSION: u32 = 9;"
        self.assertEqual(management.protocol_at("base"), management.protocol_at("target"))
        self.base.run.assert_not_called()

    @patch("management.subprocess.run")
    def test_unobservable_protocol_and_dirty_source_fail(self, command):
        command.return_value.stdout = "no protocol"
        with self.assertRaises(ValueError):
            management.protocol_at("base")
        with self.assertRaisesRegex(VmError, "not a clean checkout"):
            management.source_commit(self.source)


if __name__ == "__main__":
    unittest.main()
