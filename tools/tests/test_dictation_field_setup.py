"""Check the verifier against retained production field observations."""
import copy
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
TOOL = ROOT / "tools/release/verify_dictation_field_setup.py"
FIXTURE = ROOT / "tools/release/fixtures/dictation_field_setup_20261012/observed-fields.json"


class ObservedFieldSetupTests(unittest.TestCase):
    def test_observed_field_and_invalid_evidence(self):
        bundle = json.loads(FIXTURE.read_text())
        original = bundle["receipt"]
        idle = copy.deepcopy(original)
        idle["field_setup_journal"] = []
        mixed = copy.deepcopy(original)
        mixed["field_setup_journal"][-1] = bundle["foreign_cancel_stage"]
        inactive = copy.deepcopy(original)
        inactive["field_setup_journal"][1]["state"] = original["field_setup_journal"][0]["state"]
        missing_peer = copy.deepcopy(original)
        missing_peer["kernel_peer_pid"] = missing_peer["owned_pid"] = None
        cases = [
            ("observed", original, 0, None),
            ("idle", idle, 1, "without field/prompt/edit"),
            ("mixed", mixed, 1, "foreign or stale"),
            ("inactive", inactive, 1, "prepared target is not active"),
            ("missing-peer", missing_peer, 1, "peer missing or mismatched"),
        ]
        with tempfile.TemporaryDirectory(prefix="dictation-observations-") as directory:
            path = Path(directory) / "receipt.json"
            for optimize in (False, True):
                for name, record, expected_exit, failure in cases:
                    with self.subTest(case=name, optimize=optimize):
                        path.write_text(json.dumps(record))
                        command = [sys.executable, *(["-O"] if optimize else []), str(TOOL), str(path)]
                        result = subprocess.run(command, capture_output=True, text=True, timeout=10)
                        self.assertEqual(result.returncode, expected_exit, result.stderr + result.stdout)
                        response = json.loads(result.stdout)
                        self.assertFalse(response["speech_acceptance_verified"])
                        self.assertEqual(response["field_setup_verified"], expected_exit == 0)
                        if failure:
                            self.assertIn(failure, response["failure"])
                        else:
                            self.assertFalse(response["running_executable_readback_verified"])


if __name__ == "__main__":
    unittest.main()
