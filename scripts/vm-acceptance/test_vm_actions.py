"""TiloVm.actions reads the whole listing, not its first page."""

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))

from acceptance.vm import TiloVm, VmError  # noqa: E402


def row(n: int) -> dict:
    return {"provider": "app-actions", "id": f"app.{n}"}


CLOSER = {"provider": "desktop-windows", "id": "group.close:7"}


class ActionsPagingTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.vm = TiloVm("tilo-vm", "test-vm", "test-slot", Path(self.tmp.name) / "commands.log")
        self.calls = []

    def answer(self, pages):
        """Answer each `actions list` with the next of `pages` (a dict, or a stderr to fail with)."""
        answers = iter(pages)

        def run(cmd, **kwargs):
            self.calls.append(cmd)
            page = next(answers)
            if isinstance(page, str):
                return subprocess.CompletedProcess(cmd, 1, "", page)
            return subprocess.CompletedProcess(cmd, 0, json.dumps(page), "")

        return patch("acceptance.vm.subprocess.run", side_effect=run)

    def test_bracket_prefixed_progress_does_not_hide_the_json_response(self):
        output = '[connecting to VM]\n' + json.dumps({"actions": [CLOSER]}, indent=2) + '\nreceipt: done\n'
        with patch("acceptance.vm.subprocess.run", return_value=
                   subprocess.CompletedProcess([], 0, output, "")):
            self.assertEqual(self.vm.actions(), [CLOSER])

    def test_malformed_json_uses_the_vm_error_boundary(self):
        with patch("acceptance.vm.subprocess.run", return_value=
                   subprocess.CompletedProcess([], 0, '{"chrome":', "")):
            with self.assertRaisesRegex(VmError, "no JSON in output"):
                self.vm.state()

    def test_truncated_outer_document_does_not_return_a_nested_value(self):
        output = 'receipt: connected\n{\n  "chrome": {\n    "displays": [\n      {"id": 1}\n'
        with patch("acceptance.vm.subprocess.run", return_value=
                   subprocess.CompletedProcess([], 0, output, "")):
            with self.assertRaisesRegex(VmError, "no JSON in output"):
                self.vm.state()

    def test_restart_retries_a_malformed_state_response(self):
        replies = [subprocess.CompletedProcess([], 0, output, "") for output in
                   ("restarted", '{"chrome":', '{"chrome": {}}')]
        with (patch("acceptance.vm.subprocess.run", side_effect=replies) as run,
              patch("acceptance.vm.time.sleep")):
            self.vm.restart_session()
        self.assertEqual(run.call_count, 3)
        self.assertEqual(self.vm.requested_restarts, 1)

    def test_a_closer_past_the_first_page_is_listed(self):
        first = {"actions": [row(n) for n in range(3)], "truncated": True, "next_cursor": "3.ab"}
        last = {"actions": [CLOSER], "truncated": False, "next_cursor": None}
        with self.answer([first, last]):
            rows = self.vm.actions()
        self.assertIn(CLOSER, rows)
        self.assertEqual(len(rows), 4)
        self.assertIn("--limit", self.calls[0])
        self.assertNotIn("--cursor", self.calls[0])
        self.assertEqual(self.calls[1][self.calls[1].index("--cursor") + 1], "3.ab")

    def test_programmatic_scope_survives_paging_and_stale_cursor_restart(self):
        first = {"actions": [row(0)], "truncated": True, "next_cursor": "1.ab"}
        last = {"actions": [row(1)], "truncated": False}
        with self.answer([first, "error: stale_cursor", first, last]):
            self.assertEqual(self.vm.actions("so.tilo.terminal", programmatic=True), [row(0), row(1)])
        for cmd in self.calls:
            self.assertIn("so.tilo.terminal", cmd)
            self.assertIn("--programmatic", cmd)
        self.assertNotIn("--cursor", self.calls[2])
        self.assertIn("--cursor", self.calls[3])

    def test_a_listing_that_changed_while_paged_is_read_again_from_its_start(self):
        first = {"actions": [row(0)], "truncated": True, "next_cursor": "1.ab"}
        whole = {"actions": [row(0), CLOSER], "truncated": False}
        with self.answer([first, "error: stale_cursor: the listing changed", whole]):
            rows = self.vm.actions("group.close")
        self.assertEqual(rows, [row(0), CLOSER])
        self.assertNotIn("--cursor", self.calls[2])
        self.assertIn("group.close", self.calls[2])

    def test_cursor_cycles_fail_instead_of_looping_forever(self):
        pages = [{"actions": [row(i)], "truncated": True, "next_cursor": cursor}
                 for i, cursor in enumerate(("a", "b", "a"))]
        with self.answer(pages), self.assertRaisesRegex(VmError, "repeated.*cursor"):
            self.vm.actions()
        self.assertEqual(len(self.calls), 3)

    def test_launch_errors_use_vm_boundary_and_are_logged(self):
        for error in (FileNotFoundError("missing executable"), PermissionError("not executable")):
            with self.subTest(error=error), patch("acceptance.vm.subprocess.run", side_effect=error):
                with self.assertRaisesRegex(VmError, "could not run") as raised:
                    self.vm.state()
                self.assertIs(raised.exception.__cause__, error)
                self.assertIn(str(error), self.vm.log.read_text())

    def test_a_truncated_page_without_a_next_page_fails(self):
        with self.answer([{"actions": [row(0)], "truncated": True}]), self.assertRaises(VmError):
            self.vm.actions()

    def test_a_listing_that_never_holds_still_fails(self):
        first = {"actions": [row(0)], "truncated": True, "next_cursor": "1.ab"}
        stale = "error: stale_cursor"
        with self.answer([first, stale] * 3), self.assertRaises(VmError):
            self.vm.actions()

    def test_an_answer_without_paging_fields_is_the_whole_listing(self):
        with self.answer([{"actions": [CLOSER]}]):
            self.assertEqual(self.vm.actions(), [CLOSER])


class SplitCharacterOutputTest(unittest.TestCase):
    """A byte-counted cut of slot output (`cut -c1-400` on a session log) can
    end inside a multi-byte character; the command still answers."""

    def test_a_split_character_is_replaced_not_raised(self):
        with tempfile.TemporaryDirectory() as tmp:
            binary = Path(tmp) / "tilo-vm"
            # "née" cut after the first byte of the two-byte "é".
            binary.write_text("#!/bin/sh\nprintf 'n\\303'\n")
            binary.chmod(0o755)
            vm = TiloVm(str(binary), "test-vm", "test-slot", Path(tmp) / "commands.log")
            self.assertEqual(vm.run("slot", "exec", "cut -c1-2 log"), "n\ufffd")


if __name__ == "__main__":
    unittest.main()
