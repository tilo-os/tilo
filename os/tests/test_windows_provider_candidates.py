#!/usr/bin/env python3
"""Exercise the actual shared admission gate used by the image workflow."""
import os
from pathlib import Path
import subprocess
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "dev/check-windows-provider-candidates.sh"
FEX = "registry.tilo.so/tilo/fex@sha256:" + "1" * 64
GUEST = "registry.tilo.so/tilo/guest@sha256:" + "2" * 64


class AdmissionTests(unittest.TestCase):
    def run_gate(self, **changes):
        env = dict(os.environ, GITHUB_REF="refs/heads/main", GITHUB_EVENT_NAME="workflow_call",
                   BUILD_MODE="dev", HARDWARE_PROFILE="generic",
                   FEX_PAYLOAD_CANDIDATE=FEX, WINDOWS_GUEST_CANDIDATE=GUEST)
        env.update(changes)
        return subprocess.run(["bash", str(SCRIPT)], env=env, capture_output=True).returncode

    def test_dev_and_existing_lenovo_dispatch(self):
        self.assertEqual(self.run_gate(), 0)
        self.assertEqual(self.run_gate(GITHUB_EVENT_NAME="workflow_dispatch", BUILD_MODE="release",
                                      HARDWARE_PROFILE="lenovo-83ed"), 0)

    def test_refusal(self):
        for changes in ({"GITHUB_REF": "refs/pull/1/head"}, {"BUILD_MODE": "release"},
                        {"GITHUB_EVENT_NAME": "push"}, {"WINDOWS_GUEST_CANDIDATE": ""},
                        {"FEX_PAYLOAD_CANDIDATE": "registry.tilo.so/tilo/fex:latest"},
                        {"GITHUB_EVENT_NAME": "workflow_dispatch", "BUILD_MODE": "release"}):
            with self.subTest(changes=changes):
                self.assertNotEqual(self.run_gate(**changes), 0)


if __name__ == "__main__":
    unittest.main()
