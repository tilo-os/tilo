"""Run the real publish-mode preflight of tools/ci/publish/extension-manifests.sh with no
network or signing credentials: the script and its library run from a scratch tree whose KMS
setup, Cosign and manifest publisher are fakes."""

import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


REPO = Path(__file__).resolve().parents[2]
PINNED = "public key fixture\n"


class ExtensionPreflight(unittest.TestCase):
    def run_preflight(self, output, status):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "tools/ci/publish").mkdir(parents=True)
            for name in ("extension-manifests.sh", "lib.sh"):
                shutil.copy(REPO / "tools/ci/publish" / name, root / "tools/ci/publish" / name)
            (root / "keys").mkdir()
            (root / "keys/extensions.pub").write_text(PINNED)
            (root / "os/dev").mkdir(parents=True)
            # Identity provisioning is external to this reporting regression.
            (root / "os/dev/configure-cosign-kms.sh").write_text("trap 'touch identity-cleaned' EXIT\n")
            # Publication is the next stage: reaching it is what the preflight guards.
            (root / "tools/extensions").mkdir()
            (root / "tools/extensions/publish.sh").write_text("touch advanced\n")
            manifests = root / "manifests"
            manifests.mkdir()
            (manifests / "media-codecs.json").write_text("{}\n")
            binary = root / "bin"
            binary.mkdir()
            cosign = binary / "cosign"
            cosign.write_text(
                f"#!{sys.executable}\n"
                "import sys\n"
                "assert sys.argv[1:] == ['public-key', '--key', 'awskms:///fixture']\n"
                f"sys.stdout.write({output!r})\n"
                + ("sys.stderr.write('InvalidIdentityToken: fixture identity rejected\\n')\n" if status else "")
                + f"sys.exit({status})\n"
            )
            cosign.chmod(0o755)
            dnf = binary / "dnf"
            dnf.write_text("#!/bin/sh\nexit 0\n")
            dnf.chmod(0o755)
            scratch = root / "scratch"
            scratch.mkdir()
            result = subprocess.run(
                ["bash", str(root / "tools/ci/publish/extension-manifests.sh"), "publish", str(manifests)],
                cwd=root, capture_output=True, text=True,
                env={"PATH": f"{binary}:{os.environ['PATH']}", "TMPDIR": str(scratch),
                     "COSIGN_KEY_REF": "awskms:///fixture"},
            )
            self.assertTrue((root / "identity-cleaned").exists(), "preserve identity cleanup in the script's shell")
            self.assertEqual((root / "keys/extensions.pub").read_text(), PINNED)
            return result, (root / "advanced").exists()

    def test_retrieval_failure_is_not_reported_as_key_mismatch(self):
        result, advanced = self.run_preflight("", 1)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("InvalidIdentityToken", result.stderr)
        self.assertIn("could not read the extension-manifest public key", result.stderr)
        self.assertNotIn("does not match", result.stdout + result.stderr)
        self.assertFalse(advanced)

    def test_failed_retrieval_with_matching_partial_output_still_stops(self):
        result, advanced = self.run_preflight(PINNED, 1)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("could not read the extension-manifest public key", result.stderr)
        self.assertNotIn("does not match", result.stdout + result.stderr)
        self.assertFalse(advanced)

    def test_different_retrieved_key_still_fails_comparison(self):
        result, advanced = self.run_preflight("another public key fixture\n", 0)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("does not match", result.stdout + result.stderr)
        self.assertFalse(advanced)

    def test_empty_successful_retrieval_still_fails_comparison(self):
        result, advanced = self.run_preflight("", 0)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("does not match", result.stdout + result.stderr)
        self.assertFalse(advanced)

    def test_exact_retrieved_key_allows_the_next_stage(self):
        result, advanced = self.run_preflight(PINNED, 0)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue(advanced)


if __name__ == "__main__":
    unittest.main()
