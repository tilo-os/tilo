"""Public stress-fixture invariants; no microphone or model execution."""
import importlib.util
from pathlib import Path
import struct
import sys
import unittest

RELEASE = Path(__file__).resolve().parents[1] / "release"
sys.path.insert(0, str(RELEASE))
spec = importlib.util.spec_from_file_location("speech_stream_probe", RELEASE / "probe_speech_stream.py")
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)


class PublicSpeechStressTests(unittest.TestCase):
    def test_small_review_model_reaches_private_namespace_gate_without_execution(self):
        import os
        import subprocess
        environment = dict(os.environ)
        environment.pop('TILO_TEST_PRIVATE_NAMESPACE', None)
        result = subprocess.run(
            [sys.executable, str(RELEASE / 'probe_speech_stream.py'),
             '--manifest', '/nonexistent', '--worker', '/nonexistent',
             '--model', 'small-q5_1', '--case', 'fr_fr-0'],
            env=environment, capture_output=True, text=True, timeout=10,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('requires an unprivileged, owned hardware-free namespace', result.stderr)
        self.assertNotIn('invalid choice', result.stderr)

    def test_quantized_review_receipt_preserves_existing_models(self):
        for model, sha in probe.MODELS.items():
            self.assertEqual(probe.REVIEW_MODELS[model], sha)
        self.assertEqual(probe.REVIEW_MODELS['base-q8_0'],
                         'c577b9a86e7e048a0b7eada054f4dd79a56bbfa911fbdacf900ac5b567cbb7d9')

    def test_typed_failure_does_not_expose_peer_messages(self):
        self.assertEqual(probe.worker_failure({'kind': 'error', 'error': 'recognition'}), 'recognition')
        for reply in [{'kind': 'error', 'error': 'PRIVATE TRANSCRIPT'},
                      {'kind': 'draft', 'text': 'PRIVATE TRANSCRIPT'},
                      {'kind': 'error', 'message': 'PRIVATE TRANSCRIPT'},
                      {'kind': 'error', 'error': ['PRIVATE TRANSCRIPT']}]:
            self.assertEqual(probe.worker_failure(reply), 'unexpected_reply')

    def test_noise_is_reproducible_measured_and_bounded(self):
        pcm = struct.pack("<" + "h" * 16000, *([1000, -1000] * 8000))
        for snr in (10, 20):
            transformed, receipt = probe.synthetic_noise(pcm, snr)
            self.assertEqual(probe.synthetic_noise(pcm, snr), (transformed, receipt))
            self.assertEqual(len(transformed), len(pcm))
            self.assertNotEqual(transformed, pcm)
            self.assertAlmostEqual(receipt["achieved_snr_db"], snr, places=2)
            self.assertEqual(receipt["clipped_samples"], 0)
            self.assertIn("not real noisy speech", receipt["kind"])

    def test_clipping_is_reported_instead_of_claiming_requested_snr(self):
        pcm = struct.pack("<" + "h" * 16000, *([32767, -32768] * 8000))
        changed, receipt = probe.synthetic_noise(pcm, 10)
        self.assertEqual(len(changed), len(pcm))
        self.assertGreater(receipt["clipped_samples"], 0)
        self.assertNotAlmostEqual(receipt["achieved_snr_db"], 10, places=1)
        self.assertTrue(all(-32768 <= sample[0] <= 32767 for sample in struct.iter_unpack("<h", changed)))

    def test_unobservable_snr_or_invalid_pcm_fails(self):
        for pcm, snr in [(b"", 10), (b"\0\0" * 16000, 10), (b"\0", 10), (b"\1\0", 0), (b"\1\0", 20)]:
            with self.assertRaises((ValueError, struct.error)):
                probe.synthetic_noise(pcm, snr)


if __name__ == "__main__":
    unittest.main()
