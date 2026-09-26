#!/usr/bin/env python3
"""Keep the shell-sourced RPM pin data-only and its release gate connected."""
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]
PIN = ROOT / "os/rpms-snapshot.env"


class RpmPinContractTests(unittest.TestCase):
    def test_pin_is_an_immutable_data_only_shell_file(self) -> None:
        assignments = [line for line in PIN.read_text().splitlines()
                       if line and not line.startswith("#")]
        self.assertEqual(len(assignments), 2)
        self.assertRegex(assignments[0],
                         r"\ARPMS_REF=code\.tilo\.so/tilo/rpms@sha256:[0-9a-f]{64}\Z")
        self.assertRegex(assignments[1],
                         r"\ARPMS_SOURCE_REVISION=[0-9a-f]{40}\Z")

    def test_release_build_authenticates_pin_before_using_it(self) -> None:
        build = (ROOT / "os/dev/build-image.sh").read_text()
        workflow = (ROOT / ".forgejo/workflows/build-image.yml").read_text()
        self.assertIn('cosign verify --key "$TILO_REPO/keys/cosign.pub" "$RPMS_REF"', build)
        self.assertIn('cosign verify-attestation', build)
        self.assertIn('verify-rpm-snapshot-package-set.sh" podman "$RPMS_REF"', build)
        self.assertIn('Authenticate RPMS_REF (frozen package snapshot)', workflow)
        self.assertIn('verify-rpm-snapshot-buildkit.py "$RPMS_REF"', workflow)


if __name__ == "__main__":
    unittest.main()
