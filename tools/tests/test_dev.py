#!/usr/bin/env python3
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import dev


class DevCheckTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.out = self.root / "evidence"

    def selection(self, codes):
        return {"scope": "contract-tests", "requirements": [],
                "steps": [{"name": str(i), "argv": [sys.executable, "-c", f"raise SystemExit({code})"]}
                          for i, code in enumerate(codes)]}

    def receipt(self):
        return json.loads((self.out / "run.json").read_text())

    def test_failure_is_recorded_and_later_steps_do_not_look_successful(self):
        self.assertEqual(dev.run_plan(self.selection([3, 0]), self.out, root=self.root), 1)
        receipt = self.receipt()
        self.assertEqual(receipt["status"], "failure")
        self.assertEqual(receipt["steps"][0]["exit_code"], 3)
        self.assertEqual(receipt["unrun_steps"], ["1"])
        self.assertGreaterEqual(receipt["elapsed_seconds"], receipt["steps"][0]["elapsed_seconds"])

    def test_keep_going_retains_failure_and_records_remaining_success(self):
        self.assertEqual(dev.run_plan(self.selection([3, 0]), self.out, root=self.root, keep_going=True), 1)
        receipt = self.receipt()
        self.assertEqual(receipt["status"], "failure")
        self.assertEqual([r["status"] for r in receipt["steps"]], ["failure", "success"])
        self.assertEqual(receipt["unrun_steps"], [])

    def test_missing_prerequisite_is_a_failure_with_receipt(self):
        selection = self.selection([0])
        selection["requirements"] = ["required-fixture-tool"]
        with patch.object(dev.shutil, "which", return_value=None):
            self.assertEqual(dev.run_plan(selection, self.out, root=self.root), 2)
        self.assertEqual(self.receipt()["status"], "failure")
        self.assertEqual(self.receipt()["steps"], [])

    def test_spawn_failure_is_reported_without_a_running_step(self):
        selection = self.selection([0])
        selection["steps"][0]["argv"] = [str(self.root / "missing-command")]
        self.assertEqual(dev.run_plan(selection, self.out, root=self.root), 2)
        self.assertEqual(self.receipt()["steps"][0]["status"], "failure")

    def test_existing_evidence_is_never_overwritten(self):
        self.out.mkdir()
        (self.out / "run.json").write_text("retain this evidence")
        with self.assertRaises(FileExistsError):
            dev.run_plan(self.selection([0]), self.out, root=self.root)
        self.assertEqual((self.out / "run.json").read_text(), "retain this evidence")

    def test_real_plans_use_existing_contracts_and_make_no_release_claim(self):
        for suite in dev.SUITES:
            selection = dev.plan(suite)
            self.assertEqual(selection["scope"], "contract-tests")
            self.assertFalse(any(selection["qualification"].values()))
            self.assertEqual(len(selection["steps"]), 3)
            self.assertTrue(all((dev.ROOT / s["test_source"]).is_file() for s in selection["steps"]))


if __name__ == "__main__":
    unittest.main()
