"""Public review must not become an alternate native or release admission path."""
from pathlib import Path
import subprocess
import sys
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "release" / "qualify_sealed_speech_scope.py"


class SealedSpeechScopeTests(unittest.TestCase):
    def test_attention_proof_cannot_be_inferred_without_review_diagnostics(self):
        for options, error in [
            (["--whisper-diagnostics"], "Whisper diagnostics require explicit public review"),
            (["--whisper-public-review", "--expect-attention", "flash"],
             "attention expectation requires actual worker diagnostics"),
        ]:
            result = subprocess.run([sys.executable, str(SCRIPT), *options],
                                    capture_output=True, text=True, timeout=10)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(error, result.stderr)
            self.assertNotIn("SEALED_WORKER_SCOPE_BEFORE", result.stdout)

    def test_public_whisper_review_rejects_native_and_frozen_modes_before_execution(self):
        for options in [
            ["--native-review"], ["--lifecycle"], ["--identity-review"],
            ["--frozen-evaluation-index", "0"], ["--evaluation-profile", "context160"],
            ["--native-phase"], ["--native-low-latency"],
            ["--native-right-context", "3"], ["--phase-repeat"],
            ["--resident-second-case", "silence"],
        ]:
            with self.subTest(options=options):
                result = subprocess.run(
                    [sys.executable, str(SCRIPT), "--whisper-public-review", *options],
                    capture_output=True, text=True, timeout=10,
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("public Whisper review excludes", result.stderr)
                self.assertNotIn("SEALED_WORKER_SCOPE_BEFORE", result.stdout)

    def test_alternate_public_fixture_requires_explicit_review(self):
        result = subprocess.run(
            [sys.executable, str(SCRIPT), "--case", "de_de-0"],
            capture_output=True, text=True, timeout=10,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("explicit public review required", result.stderr)
        self.assertNotIn("SEALED_WORKER_SCOPE_BEFORE", result.stdout)

    def test_alternate_model_requires_public_review_before_execution(self):
        for model in ["tiny", "small-q5_1"]:
            with self.subTest(model=model):
                result = subprocess.run(
                    [sys.executable, str(SCRIPT), "--whisper-model", model],
                    capture_output=True, text=True, timeout=10,
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("alternate Whisper model requires explicit public review", result.stderr)
                self.assertNotIn("SEALED_WORKER_SCOPE_BEFORE", result.stdout)

    def test_allocator_override_requires_public_review_before_execution(self):
        result = subprocess.run(
            [sys.executable, str(SCRIPT), "--whisper-allocator", "default"],
            capture_output=True, text=True, timeout=10,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("alternate Whisper allocator requires explicit public review", result.stderr)
        self.assertNotIn("SEALED_WORKER_SCOPE_BEFORE", result.stdout)


if __name__ == "__main__":
    unittest.main()
