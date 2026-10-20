"""Experimental adapter receipt/CPU accounting safeguards; no inference or mic."""
import hashlib
import json
import os
from unittest.mock import patch
import importlib.util
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from hang import HANG_BOUND

RELEASE = Path(__file__).resolve().parents[1] / 'release'
sys.path.insert(0, str(RELEASE))
spec = importlib.util.spec_from_file_location('candidate_probe', RELEASE / 'probe_asr_candidate.py')
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)


class CandidateProbeTests(unittest.TestCase):
    def test_actual_native_thread_excess_stops_owned_process_before_deadline(self):
        code = ('import threading,time; '
                '[threading.Thread(target=lambda:time.sleep(30),daemon=True).start() '
                'for _ in range(3)]; time.sleep(30)')
        executable = Path(sys.executable).stat()
        result = probe.observe([sys.executable, '-c', code],
                               {min(os.sched_getaffinity(0))},
                               (executable.st_dev, executable.st_ino), max_threads=2)
        self.assertEqual(result['guarded'], 'native_thread_limit')
        self.assertNotEqual(result['exit'], 0)
        self.assertGreater(result['memory_peak']['Threads'], 2)
        self.assertLess(result['wall_seconds'], 10)
        self.assertIsNone(result['cpu_seconds'])

    def test_invalid_thread_limit_fails_before_process_launch(self):
        for limit in (0, -1, True, 2.5, '2'):
            with self.assertRaisesRegex(ValueError, 'native thread limit'):
                probe.observe(['/nonexistent'], {0}, max_threads=limit)

    def test_receipted_executable_is_observed_despite_changed_thread_name(self):
        code = ('import ctypes,time; '
                'ctypes.CDLL(None).prctl(15,b"renamed-runtime",0,0,0); '
                'print("ready",flush=True); time.sleep(30)')
        child = subprocess.Popen([sys.executable, '-c', code],
                                 stdout=subprocess.PIPE, text=True)
        try:
            self.assertEqual(child.stdout.readline().strip(), 'ready')
            info = Path(sys.executable).stat()
            identity = (info.st_dev, info.st_ino)
            self.assertEqual(probe.subjects(child.pid), [])
            self.assertEqual(probe.subjects(child.pid, identity), [child.pid])
            self.assertEqual(probe.subjects(child.pid, (identity[0], identity[1] + 1)), [])
        finally:
            child.terminate()
            child.wait(timeout=HANG_BOUND)
            child.stdout.close()

    def test_actual_child_time_receipt_not_outer_wrapper_accounting(self):
        # The minimal policy image has no GNU time. Measure a real child's CPU
        # with the standard library; the remote native probe still requires
        # /usr/bin/time and never accepts wrapper-only accounting.
        child_code = ('import json,resource; sum(range(200000)); '
                      'r=resource.getrusage(resource.RUSAGE_SELF); '
                      'print("ASR_RESOURCE " + json.dumps({"user":r.ru_utime,"system":r.ru_stime}))')
        child = subprocess.run([sys.executable, '-c', child_code],
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, check=True)
        receipt = json.loads(child.stdout.split(' ', 1)[1])
        self.assertEqual(probe.measured_cpu(child.stdout, True), receipt['user'] + receipt['system'])
        self.assertGreater(probe.measured_cpu(child.stdout, True), 0)
        self.assertIsNone(probe.measured_cpu('guarded before native exit', False))
        with self.assertRaises(RuntimeError):
            probe.measured_cpu('', True)
        with self.assertRaises(RuntimeError):
            probe.measured_cpu(child.stdout + child.stdout, True)

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
