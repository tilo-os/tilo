#!/usr/bin/env python3
"""The profile audit must reject a successful but unobserved benchmark."""

import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("audit", Path(__file__).with_name("typing-iai-check.py"))
audit = importlib.util.module_from_spec(spec)
spec.loader.exec_module(audit)


class ProfileAuditTests(unittest.TestCase):
    def fixture(self, root, function="<tilo_ui::headless::FrameHarness>::key"):
        for name, size in audit.CASES:
            directory = root / f"{name}.{size}"
            directory.mkdir()
            (directory / "part.out").write_text(
                "positions: line\nevents: " + " ".join(audit.EVENTS)
                + f"\nsummary: 9\nfn={function}\n1 9\n"
            )
            (directory / "summary.json").write_text(json.dumps({
                "function_name": name, "id": size,
                "profiles": [{
                    "tool": "Callgrind", "out_paths": ["/target/part.out"],
                    "summaries": {"total": {"summary": {"Callgrind": {
                        event: {"metrics": {"Left": {"Int": 9 if event == "Ir" else 0}}}
                        for event in audit.EVENTS
                    }}}},
                }],
            }))

    def test_observed_and_reconciled(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.fixture(root)
            self.assertEqual(audit.check_profiles(root), len(audit.CASES))

    def test_wrapper_only_is_not_a_pass(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.fixture(root, "typing::wrapper")
            with self.assertRaisesRegex(ValueError, "did not observe"):
                audit.check_profiles(root)

    def test_bad_raw_totals_are_not_a_pass(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.fixture(root)
            path = next(root.rglob("part.out"))
            path.write_text(path.read_text().replace("summary: 9", "summary: 10"))
            with self.assertRaisesRegex(ValueError, "do not reconcile"):
                audit.check_profiles(root)

    def test_json_mismatch_is_not_a_pass(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.fixture(root)
            path = next(root.rglob("summary.json"))
            data = json.loads(path.read_text())
            data["profiles"][0]["summaries"]["total"]["summary"]["Callgrind"]["Ir"]["metrics"]["Left"]["Int"] = 10
            path.write_text(json.dumps(data))
            with self.assertRaisesRegex(ValueError, "JSON total differs"):
                audit.check_profiles(root)

    def test_missing_cases_are_not_a_pass(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(ValueError, "missing typing cases"):
                audit.check_profiles(Path(directory))


if __name__ == "__main__":
    unittest.main()
