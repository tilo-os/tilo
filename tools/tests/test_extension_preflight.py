"""Run the actual workflow preflight with no network or signing credentials."""

import os
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path


REPO = Path(__file__).resolve().parents[2]
PINNED = "public key fixture\n"


class ExtensionPreflight(unittest.TestCase):
    def run_preflight(self, output, status):
        workflow = (REPO / ".forgejo/workflows/extension-manifests.yml").read_text()
        step = workflow.split("      - name: Sign and publish each resolved manifest\n", 1)[1]
        program = textwrap.dedent(step.split("        run: |\n", 1)[1].split("      - uses:", 1)[0])
        # Run through the real preflight, stopping before manifest publication.
        program = program.split("mapfile -t manifests", 1)[0] + "touch advanced\n"
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "keys").mkdir()
            (root / "keys/extensions.pub").write_text(PINNED)
            (root / "os/dev").mkdir(parents=True)
            # Identity provisioning is external to this reporting regression.
            (root / "os/dev/configure-cosign-kms.sh").write_text("trap 'touch identity-cleaned' EXIT\n")
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
            runner = root / "runner"
            runner.mkdir()
            result = subprocess.run(
                ["bash", "-c", program], cwd=root, capture_output=True, text=True,
                env={"PATH": f"{binary}:{os.environ['PATH']}", "RUNNER_TEMP": str(runner),
                     "COSIGN_KEY_REF": "awskms:///fixture"},
            )
            self.assertTrue((root / "identity-cleaned").exists(), "preserve identity cleanup in the parent shell")
            self.assertEqual((root / "keys/extensions.pub").read_text(), PINNED)
            self.assertEqual(list(runner.iterdir()), [], "preflight must clean temporary key files")
            return result, (root / "advanced").exists()

    def test_retrieval_failure_is_not_reported_as_key_mismatch(self):
        result, advanced = self.run_preflight("", 1)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("InvalidIdentityToken", result.stderr)
        self.assertIn("could not retrieve", result.stdout + result.stderr)
        self.assertNotIn("does not match", result.stdout + result.stderr)
        self.assertFalse(advanced)

    def test_failed_retrieval_with_matching_partial_output_still_stops(self):
        result, advanced = self.run_preflight(PINNED, 1)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("could not retrieve", result.stdout + result.stderr)
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
