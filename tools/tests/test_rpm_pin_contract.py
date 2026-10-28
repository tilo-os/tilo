#!/usr/bin/env python3
"""Frozen RPM snapshots resolve by fingerprint; builds authenticate the result."""
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]


class RpmResolveContractTests(unittest.TestCase):
    def test_no_hand_kept_rpms_pin_file(self) -> None:
        self.assertFalse((ROOT / "os/rpms-snapshot.env").exists())

    def test_fingerprint_and_resolve_tools_exist(self) -> None:
        fingerprint = ROOT / "os/dev/rpm-snapshot-fingerprint.py"
        resolve = ROOT / "os/dev/resolve-rpm-snapshot.sh"
        self.assertTrue(fingerprint.is_file())
        self.assertTrue(resolve.is_file())
        self.assertNotIn("rpms-snapshot.env", resolve.read_text())

    def test_release_build_resolves_before_using_rpms(self) -> None:
        build = (ROOT / "os/dev/build-image.sh").read_text()
        self.assertIn('resolve-rpm-snapshot.sh', build)
        self.assertIn('cosign verify --key "$TILO_REPO/keys/cosign.pub" "$RPMS_REF"', build)
        self.assertIn('cosign verify-attestation', build)
        self.assertIn('verify-rpm-snapshot-package-set.sh" podman "$RPMS_REF"', build)


if __name__ == "__main__":
    unittest.main()
