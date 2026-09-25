#!/usr/bin/env python3
"""Exercise the shared signing preflight against a stub Cosign."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "os/dev/require-cosign-authority.sh"
PINNED = (ROOT / "keys/cosign.pub").read_text()


class RequireCosignAuthorityTests(unittest.TestCase):
    def run_preflight(self, key_ref, public_key):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            (work / "key.pub").write_text(public_key)
            cosign = work / "cosign"
            cosign.write_text(
                "#!/bin/sh\n"
                '[ "$1" = public-key ] && [ "$2" = --key ] || exit 9\n'
                f'cat "{work}/key.pub"\n'
            )
            cosign.chmod(0o755)
            env = {
                "PATH": f"{work}:/usr/bin:/bin",
                "RUNNER_TEMP": str(work),
                "COSIGN_KEY_REF": key_ref,
            }
            result = subprocess.run(
                [str(SCRIPT)], env=env, cwd=work, capture_output=True, text=True
            )
            leftovers = list(work.glob("signing-authority.*"))
            return result, leftovers

    def test_accepts_the_pinned_kms_key(self):
        result, leftovers = self.run_preflight("gcpkms://projects/p/keys/k", PINNED)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(leftovers, [])

    def test_rejects_a_different_kms_key(self):
        result, leftovers = self.run_preflight("hashivault://release", PINNED + "x")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("does not match keys/cosign.pub", result.stderr)
        self.assertEqual(leftovers, [])

    def test_rejects_an_exportable_key_before_contacting_cosign(self):
        for key_ref in ("", "cosign.key", "env://COSIGN_PRIVATE_KEY"):
            result, _ = self.run_preflight(key_ref, PINNED)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("non-exportable KMS/HSM key", result.stderr)


if __name__ == "__main__":
    unittest.main()
