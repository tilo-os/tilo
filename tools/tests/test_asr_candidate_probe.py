"""Experimental adapter receipt/CPU accounting safeguards; no inference or mic."""
import hashlib
import os
from unittest.mock import patch
import importlib.util
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

RELEASE = Path(__file__).resolve().parents[1] / 'release'
sys.path.insert(0, str(RELEASE))
spec = importlib.util.spec_from_file_location('candidate_probe', RELEASE / 'probe_asr_candidate.py')
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)


class CandidateProbeTests(unittest.TestCase):
    def test_actual_child_time_receipt_not_outer_wrapper_accounting(self):
        child = subprocess.run(['/usr/bin/time', '-f', 'ASR_RESOURCE {"user":%U,"system":%S}',
                                sys.executable, '-c', 'sum(range(10000))'],
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, check=True)
        self.assertIsInstance(probe.measured_cpu(child.stderr, True), float)
        self.assertGreaterEqual(probe.measured_cpu(child.stderr, True), 0)
        self.assertIsNone(probe.measured_cpu('guarded before native exit', False))
        with self.assertRaises(RuntimeError):
            probe.measured_cpu('', True)
        with self.assertRaises(RuntimeError):
            probe.measured_cpu(child.stderr + child.stderr, True)

    def test_malformed_or_unobservable_resource_receipt_cannot_be_scored(self):
        for value in ['{"user":-1,"system":0}', '{"user":NaN,"system":0}',
                      '{"user":true,"system":0}', '{"user":1,"system":"private"}',
                      '{"cpu":1}', '{"user":1,"system":0,"other":2}']:
            with self.assertRaises(ValueError):
                probe.measured_cpu('ASR_RESOURCE ' + value, True)

    def test_disappearing_proc_subject_is_tolerated_but_unobserved_decoder_fails(self):
        with patch('pathlib.Path.read_text', side_effect=ProcessLookupError('owned process exited')):
            self.assertEqual(probe.subjects(123), [])
        with self.assertRaisesRegex(RuntimeError, 'cannot observe actual decoder'):
            probe.observe([sys.executable, '-c', 'print("fixture")'],
                          {min(os.sched_getaffinity(0))})

    def test_exact_public_artifact_receipts_required(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'public'
            path.write_bytes(b'fixture')
            digest = hashlib.sha256(b'fixture').hexdigest()
            probe.receipt(path, 7, digest)
            for size, sha in [(8, digest), (7, 'f' * 64)]:
                with self.assertRaises(ValueError):
                    probe.receipt(path, size, sha)


if __name__ == '__main__':
    unittest.main()
