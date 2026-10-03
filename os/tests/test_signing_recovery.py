"""Focused state-machine tests for explicit local signing recovery."""
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'dev'))
import signing_spool as spool


def restart_fixture(test):
    """Give the next subTest phase of a merged test the fresh fixture it had
    as its own test: tear down, run cleanups, reset instance state, set up."""
    test.tearDown()
    test.doCleanups()
    fresh = type(test)(test._testMethodName).__dict__
    keep = {key: test.__dict__[key] for key in ("_outcome", "_subtest") if key in test.__dict__}
    test.__dict__.clear()
    test.__dict__.update(fresh, **keep)
    test.setUp()


class SigningRecoveryTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(dir='/var/lib', prefix='tilo-recovery-test-')
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.response = self.root / 'response'
        self.response.mkdir(mode=0o700)
        self.signer = self.root / 'signer'
        self.signer.write_bytes(b'#!/bin/sh\nexit 1\n')
        self.signer.chmod(0o755)
        self.data = b'x' * 40
        self.verification = b'fixture public verification key'
        self.request = {
            'schema': 1,
            'request_id': '1' * 32,
            'context_sha256': '2' * 64,
            'operation': 'sign-sha256-rsa-pkcs1',
            'input_sha256': spool.digest(self.data),
            'input_bytes': len(self.data),
        }
        used = self.root / self.request['context_sha256']
        used.mkdir(mode=0o700)
        (used / ('request-' + self.request['request_id'])).mkdir(mode=0o700)
        self.reservation = used / 'authorization-target-test'
        self.reservation.mkdir(mode=0o700)
        (self.reservation / 'request.json').write_bytes(spool.canonical(self.request))
        (self.reservation / 'input').write_bytes(self.data)
        (self.reservation / 'verification.pem').write_bytes(self.verification)
        (self.reservation / 'output').write_bytes(b'')

    def recover(self):
        return spool.recover_local_request(
            self.reservation, self.response, self.signer, self.request['request_id'],
            self.request['operation'], self.request['input_sha256'], spool.digest(self.verification))

    def test_interrupted_signing_recovery(self):
        with self.subTest(phase="exact_interruption_retries_once_verifies_and_publishes"):
            calls = []

            def run(arguments, **_kwargs):
                calls.append(arguments)
                if arguments[0] == str(self.signer):
                    self.assertTrue((self.reservation / 'recovery.json').exists())
                    self.assertFalse((self.reservation / 'output').exists())
                    Path(arguments[-1]).write_bytes(b's' * 256)
                else:
                    self.assertEqual(arguments[0:2], ['/usr/local/bin/tilo-crypto', 'verify-rsa-sha256'])
                return b''

            with patch.object(spool.signer, 'run', side_effect=run):
                receipt = self.recover()
            self.assertTrue(receipt['recovered'])
            self.assertEqual(len(calls), 2)
            self.assertEqual((self.response / 'output').read_bytes(), b's' * 256)
            self.assertEqual(spool.document(self.response / 'response.json'),
                             spool.document(self.reservation / 'complete.json'))
        restart_fixture(self)
        with self.subTest(phase="completed_and_nonzero_outputs_are_rejected_without_signing"):
            for kind in ('completed', 'nonzero'):
                with self.subTest(kind=kind):
                    if kind == 'completed':
                        (self.reservation / 'complete.json').write_bytes(b'{}')
                    else:
                        (self.reservation / 'output').write_bytes(b'ambiguous')
                    with patch.object(spool.signer, 'run') as run, self.assertRaises(ValueError):
                        self.recover()
                    run.assert_not_called()
                    if kind == 'completed':
                        (self.reservation / 'complete.json').unlink()
                    else:
                        (self.reservation / 'output').write_bytes(b'')
        restart_fixture(self)
        with self.subTest(phase="exact_operator_pins_are_required_without_signing"):
            arguments = [self.reservation, self.response, self.signer, self.request['request_id'],
                         self.request['operation'], self.request['input_sha256'], spool.digest(self.verification)]
            for index, replacement in ((3, 'f' * 32), (4, 'sign-efi'), (5, 'f' * 64), (6, 'f' * 64)):
                changed = list(arguments)
                changed[index] = replacement
                with self.subTest(index=index), patch.object(spool.signer, 'run') as run, self.assertRaises(ValueError):
                    spool.recover_local_request(*changed)
                run.assert_not_called()
        restart_fixture(self)
        with self.subTest(phase="failed_retry_is_durably_marked_and_cannot_retry_again"):
            with patch.object(spool.signer, 'run', side_effect=ValueError('interrupted')) as run:
                with self.assertRaisesRegex(ValueError, 'interrupted'):
                    self.recover()
            self.assertEqual(run.call_count, 1)
            self.assertTrue((self.reservation / 'recovery.json').exists())
            with patch.object(spool.signer, 'run') as second_run, self.assertRaisesRegex(ValueError, 'exact incomplete'):
                self.recover()
            second_run.assert_not_called()


if __name__ == '__main__':
    unittest.main()
