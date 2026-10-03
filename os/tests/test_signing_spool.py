"""Real fixture-key signing and hostile handoff tests; no persistent key access."""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import sys
import subprocess
import tempfile
import threading
import time
import unittest
from unittest.mock import patch
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'dev'))
import signing_spool as spool
from test_development_boot import synthetic_uki

SPEC = importlib.util.spec_from_file_location('authorize', Path(__file__).resolve().parents[1] / 'dev/authorize-signing-input.py')
authorize = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(authorize)


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


class SigningSpoolTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        # Test the actual signer implementation; do not replace its backend.
        # Run in the prepared native tool container, never install test tools
        # globally or substitute a legacy external signer on newer main.
        required = ['/usr/local/bin/tilo-crypto', '/usr/bin/unshare', '/usr/bin/mount']
        try:
            if os.getuid() != 0:
                raise ValueError('already-root native Linux required')
            spool.signer.trusted(Path('/var/lib'), directory=True)
            for tool in required:
                # Distribution /usr/bin aliases are allowed for independent
                # fixture verifiers. Production crypto is strictly canonical.
                path = Path(tool) if tool == '/usr/local/bin/tilo-crypto' else Path(tool).resolve()
                if tool == '/usr/local/bin/tilo-crypto':
                    spool.signer.trusted(path)
                elif not path.is_file() or path.stat().st_uid != 0 or path.stat().st_mode & 0o022:
                    raise ValueError('fixture tool is not root-owned and protected')
                if not os.access(path, os.X_OK):
                    raise ValueError('required tool is not executable')
        except (OSError, ValueError) as error:
            raise RuntimeError('signing-spool prerequisite missing: prepared native tools, root-owned '
                               '/usr/local/bin/tilo-crypto when required by signer, and non-group-writable '
                               '/var/lib; run in the isolated tools container described in REMOTE-SIGNING.md') from error
        probe = subprocess.run(['/usr/bin/unshare', '--mount', '--propagation', 'private', '/usr/bin/true'],
                               capture_output=True, timeout=10)
        if probe.returncode:
            raise RuntimeError('signing-spool prerequisite missing: isolated mount namespace capability '
                               '(SYS_ADMIN in the disposable tools container)')
        cls.tmp = tempfile.TemporaryDirectory(prefix='tilo-spool-test-', dir='/var/lib')
        cls.root = Path(cls.tmp.name)
        cls.home = cls.root / 'fixture-signer'
        cls.home.mkdir(mode=0o700)
        source = Path(__file__).resolve().parents[1] / 'dev/development-signer.py'
        (cls.home / 'signer').write_text(source.read_text())
        (cls.home / 'signer').chmod(0o755)
        (cls.home / 'purpose').write_bytes(spool.signer.PURPOSE)
        crypto = '/usr/local/bin/tilo-crypto'
        spool.signer.run([crypto, 'generate-tpm-authority-key',
                          str(cls.home / 'pcr-signing.key'), str(cls.home / 'pcr.pub')])
        spool.signer.run([crypto, 'generate-secure-boot-cert', 'Throwaway EFI test',
                          str(cls.home / 'efi-signing.key'), str(cls.home / 'efi-signing.crt')])

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def setUp(self):
        self.work = Path(tempfile.mkdtemp(dir=self.root))
        for name in ('spool', 'state', 'out'):
            (self.work / name).mkdir(mode=0o700)
        self.context = {'schema': 1, 'job_id': 'fixture-job', 'pod_uid': 'fixture-pod', 'container_id': 'fixture-container',
                        'image_id': 'sha256:' + 'a' * 64, 'revision': 'a' * 40, 'architecture': 'aarch64',
                        'manifest_sha256': 'b' * 64, 'target': 'registry.invalid/tilo/test:aarch64'}
        self.config = self.work / 'bridge.json'
        self.config.write_bytes(spool.canonical({'schema': 1, 'context': self.context, 'spool': str(self.work / 'spool'),
            'public_certificate': str(self.home / 'efi-signing.crt'),
            'certificate_sha256': spool.digest((self.home / 'efi-signing.crt').read_bytes()), 'timeout_seconds': 3}))

    def policy(self, data, purpose='pcr-policy', operation='sign-sha256-rsa-pkcs1'):
        value = {'schema': 1, 'context': self.context, 'authorized_inputs': [{
            'id': 'fixture-operation', 'purpose': purpose, 'operation': operation,
            'input_sha256': spool.digest(data), 'input_bytes': len(data)}]}
        for name, filename in (('efi_certificate', 'efi-signing.crt'), ('pcr_public_key', 'pcr.pub')):
            path = self.home / filename
            value[name] = {'path': str(path), 'sha256': spool.digest(path.read_bytes())}
        path = self.work / 'policy.json'
        path.write_bytes(spool.canonical(value))
        return path

    def request(self, data, nonce='1' * 32, operation='sign-sha256-rsa-pkcs1'):
        directory = self.work / 'spool' / nonce
        directory.mkdir(mode=0o700)
        (directory / 'input').write_bytes(data)
        value = {'schema': 1, 'request_id': nonce, 'context_sha256': spool.context(self.context),
                 'operation': operation, 'input_sha256': spool.digest(data), 'input_bytes': len(data)}
        (directory / 'request.json').write_bytes(spool.canonical(value))
        return directory

    def service(self, request, policy):
        return spool.service_local_request(request, policy, self.work / 'state', request, self.home / 'signer')

    def interrupted_reservation(self, data=b'x' * 40, nonce='3' * 32,
                                operation='sign-sha256-rsa-pkcs1', identifier='fixture-recovery'):
        binding = spool.context(self.context)
        used = self.work / 'state' / binding
        used.mkdir(mode=0o700, exist_ok=True)
        (used / ('request-' + nonce)).mkdir(mode=0o700)
        reservation = used / ('authorization-' + identifier)
        reservation.mkdir(mode=0o700)
        request = {'schema': 1, 'request_id': nonce, 'context_sha256': binding,
                   'operation': operation, 'input_sha256': spool.digest(data),
                   'input_bytes': len(data)}
        (reservation / 'request.json').write_bytes(spool.canonical(request))
        (reservation / 'input').write_bytes(data)
        verification = self.home / ('efi-signing.crt' if operation == 'sign-efi' else 'pcr.pub')
        (reservation / 'verification.pem').write_bytes(verification.read_bytes())
        (reservation / 'output').write_bytes(b'')
        return reservation, request, spool.digest(verification.read_bytes())

    def recover(self, reservation, request, verification_sha256):
        return spool.recover_local_request(
            reservation, self.work / 'out', self.home / 'signer', request['request_id'],
            request['operation'], request['input_sha256'], verification_sha256)

    def test_remote_adapter_and_response_verification(self):
        with self.subTest(phase="real_rsa_remote_adapter_roundtrip_and_replay_rejection"):
            data = b'x' * 40
            source = self.work / 'policy-message'
            source.write_bytes(data)
            destination = self.work / 'out' / 'signature'
            errors = []
            def remote():
                try:
                    spool.remote_sign(self.config, 'sign-sha256-rsa-pkcs1', source, destination)
                except Exception as error:
                    errors.append(error)
            thread = threading.Thread(target=remote)
            thread.start()
            deadline = time.monotonic() + 2
            requests = []
            while not requests and time.monotonic() < deadline:
                requests = list((self.work / 'spool').glob('*/request.json'))
                time.sleep(0.01)
            self.assertEqual(len(requests), 1)
            policy = self.policy(data)
            receipt = self.service(requests[0].parent, policy)
            thread.join(timeout=5)
            self.assertFalse(thread.is_alive())
            self.assertEqual(errors, [])
            self.assertFalse(receipt['private_keys_transferred'])
            spool.signer.run(['/usr/local/bin/tilo-crypto', 'verify-rsa-sha256',
                              str(self.home / 'pcr.pub'), str(source), str(destination)])
            with self.assertRaises(FileExistsError):
                self.service(requests[0].parent, policy)
            with self.assertRaisesRegex(ValueError, 'already consumed'):
                self.service(self.request(data, '2' * 32), policy)
        restart_fixture(self)
        with self.subTest(phase="actual_efi_response_verified_with_fixture_certificate"):
            data = synthetic_uki('aarch64')
            request = self.request(data, operation='sign-efi')
            self.service(request, self.policy(data, 'uki', 'sign-efi'))
            spool.signer.run(['/usr/local/bin/tilo-crypto', 'verify-efi',
                              str(self.home / 'efi-signing.crt'), str(request / 'output')])
        restart_fixture(self)
        with self.subTest(phase="cross_request_response_rejected_without_output"):
            source = self.work / 'message'
            source.write_bytes(b'x' * 40)
            destination = self.work / 'out' / 'signature'
            errors = []
            def remote():
                try:
                    spool.remote_sign(self.config, 'sign-sha256-rsa-pkcs1', source, destination)
                except Exception as error:
                    errors.append(error)
            thread = threading.Thread(target=remote)
            thread.start()
            deadline = time.monotonic() + 2
            requests = []
            while not requests and time.monotonic() < deadline:
                requests = list((self.work / 'spool').glob('*/request.json'))
                time.sleep(0.01)
            self.assertEqual(len(requests), 1)
            directory = requests[0].parent
            metadata = json.loads(requests[0].read_text())
            response = {k: metadata[k] for k in ('schema', 'request_id', 'context_sha256', 'operation', 'input_sha256')}
            response.update(request_id='0' * 32, output_sha256='0' * 64, output_bytes=256)
            spool.publish(directory / 'response.json', spool.canonical(response))
            thread.join(timeout=5)
            self.assertFalse(thread.is_alive())
            self.assertEqual(len(errors), 1)
            self.assertIsInstance(errors[0], ValueError)
            self.assertFalse(destination.exists())
        restart_fixture(self)
        with self.subTest(phase="authorization_checks_exact_hash_architecture_and_seal"):
            source = self.work / 'target.efi'
            data = synthetic_uki('aarch64')
            source.write_bytes(data)
            kwargs = dict(identifier='test-uki', purpose='uki', source=source, expected_sha256=spool.digest(data),
                          architecture='aarch64', prepared_input=self.work / 'out' / 'prepared', generation=7, composefs_digest='a' * 128)
            row = authorize.authorize(**kwargs)
            self.assertEqual(row['input_sha256'], spool.digest(data))
            for changes in ({'expected_sha256': '0' * 64}, {'architecture': 'x86_64'}, {'composefs_digest': 'b' * 128}):
                with self.assertRaises(ValueError):
                    authorize.authorize(**(kwargs | changes))

    def test_reservation_recovery(self):
        with self.subTest(phase="interrupted_reservation_recovery_signs_verifies_and_publishes_once"):
            reservation, request, verification_sha256 = self.interrupted_reservation()
            receipt = self.recover(reservation, request, verification_sha256)
            self.assertTrue(receipt['recovered'])
            self.assertEqual(spool.document(reservation / 'complete.json'),
                             spool.document(self.work / 'out' / 'response.json'))
            self.assertEqual((reservation / 'output').read_bytes(), (self.work / 'out' / 'output').read_bytes())
            spool.signer.run(['/usr/local/bin/tilo-crypto', 'verify-rsa-sha256',
                              str(self.home / 'pcr.pub'), str(reservation / 'input'),
                              str(self.work / 'out' / 'output')])
            with self.assertRaisesRegex(ValueError, 'exact incomplete'):
                self.recover(reservation, request, verification_sha256)
        restart_fixture(self)
        with self.subTest(phase="recovery_rejects_completed_nonzero_and_mismatched_reservations_before_signing"):
            cases = ('complete', 'nonzero', 'request-id', 'input-hash', 'verification-hash')
            for index, case in enumerate(cases):
                with self.subTest(case=case):
                    nonce = format(index + 4, 'x') * 32
                    reservation, request, verification_sha256 = self.interrupted_reservation(
                        nonce=nonce, identifier='recovery-' + case)
                    if case == 'complete':
                        (reservation / 'complete.json').write_bytes(b'{}')
                    elif case == 'nonzero':
                        (reservation / 'output').write_bytes(b'ambiguous')
                    elif case == 'request-id':
                        request = request | {'request_id': 'f' * 32}
                    elif case == 'input-hash':
                        request = request | {'input_sha256': 'f' * 64}
                    else:
                        verification_sha256 = 'f' * 64
                    with patch.object(spool.signer, 'run') as run, self.assertRaises((OSError, ValueError)):
                        self.recover(reservation, request, verification_sha256)
                    run.assert_not_called()
        restart_fixture(self)
        with self.subTest(phase="recovery_commits_retry_marker_before_signer_and_never_retries_it"):
            reservation, request, verification_sha256 = self.interrupted_reservation()
            with patch.object(spool.signer, 'run', side_effect=ValueError('simulated interruption')) as run:
                with self.assertRaisesRegex(ValueError, 'simulated interruption'):
                    self.recover(reservation, request, verification_sha256)
            self.assertEqual(run.call_count, 1)
            self.assertEqual(spool.document(reservation / 'recovery.json')['request_id'], request['request_id'])
            with patch.object(spool.signer, 'run') as second_run, self.assertRaisesRegex(ValueError, 'exact incomplete'):
                self.recover(reservation, request, verification_sha256)
            second_run.assert_not_called()
        restart_fixture(self)
        with self.subTest(phase="reservation_is_fsynced_before_private_key_use"):
            data = b'x' * 40
            request = self.request(data)
            policy = self.policy(data)
            synced = []
            original_sync = spool.sync_directory
            original_run = spool.signer.run
            def sync(directory):
                synced.append(Path(directory))
                original_sync(directory)
            def run(arguments, **kwargs):
                if arguments[0] == str(self.home / 'signer'):
                    used = self.work / 'state' / spool.context(self.context)
                    self.assertIn(self.work / 'state', synced)
                    self.assertIn(used, synced)
                    self.assertIn(used / 'authorization-fixture-operation', synced)
                return original_run(arguments, **kwargs)
            with patch.object(spool, 'sync_directory', side_effect=sync), patch.object(spool.signer, 'run', side_effect=run):
                self.service(request, policy)

    def test_input_and_publication_safety(self):
        with self.subTest(phase="mismatched_input_and_context_never_reserve_authorization"):
            data = b'x' * 40
            policy = self.policy(data)
            request = self.request(data)
            (request / 'input').write_bytes(b'y' * 40)
            with self.assertRaisesRegex(ValueError, 'changed'):
                self.service(request, policy)
            (request / 'input').write_bytes(data)
            metadata = json.loads((request / 'request.json').read_text())
            metadata['context_sha256'] = '0' * 64
            (request / 'request.json').write_bytes(spool.canonical(metadata))
            with self.assertRaisesRegex(ValueError, 'another signing context'):
                self.service(request, policy)
            self.assertEqual(list((self.work / 'state').iterdir()), [])
        restart_fixture(self)
        with self.subTest(phase="unapproved_bytes_symlink_and_size_fail_closed"):
            data = b'x' * 40
            policy = self.policy(data)
            request = self.request(b'y' * 40)
            with self.assertRaisesRegex(ValueError, 'not explicitly authorized'):
                self.service(request, policy)
            (request / 'input').unlink()
            (request / 'input').symlink_to(self.home / 'pcr-signing.key')
            with self.assertRaises(OSError):
                self.service(request, policy)
            metadata = json.loads((request / 'request.json').read_text())
            metadata['input_bytes'] = spool.MAX_INPUT + 1
            (request / 'request.json').write_bytes(spool.canonical(metadata))
            with self.assertRaisesRegex(ValueError, 'invalid signing request'):
                self.service(request, policy)
        restart_fixture(self)
        with self.subTest(phase="publication_is_single_link_before_directory_sync_and_never_overwrites"):
            path = self.work / 'out' / 'request.json'
            original_sync = spool.sync_directory
            observed = []
            def observe(directory):
                if path.exists():
                    observed.append((path.stat().st_nlink, spool.document(path)))
                original_sync(directory)
            with patch.object(spool, 'sync_directory', side_effect=observe):
                spool.publish(path, b'{"schema":1}')
            self.assertEqual(observed, [(1, {'schema': 1})])
            with self.assertRaises(OSError):
                spool.publish(path, b'{"schema":2}')
            self.assertEqual(spool.document(path), {'schema': 1})
        restart_fixture(self)
        with self.subTest(phase="readonly_signer_home_real_private_mount_namespace"):
            source = self.work / 'message'
            source.write_bytes(b'x' * 40)
            output = self.work / 'out' / 'readonly-signature'
            script = """import subprocess,sys
home,source,destination=sys.argv[1:]
subprocess.run(['mount','--bind',home,home],check=True)
subprocess.run(['mount','-o','remount,bind,ro',home],check=True)
subprocess.run([home+'/signer','sign-sha256-rsa-pkcs1',source,destination],check=True)
"""
            result = subprocess.run(['/usr/bin/unshare', '--mount', '--propagation', 'private',
                                     '/usr/bin/python3', '-c', script, str(self.home), str(source), str(output)],
                                    capture_output=True, timeout=30)
            self.assertEqual(result.returncode, 0, 'fixture signer failed with actual read-only home')
            spool.signer.run(['/usr/local/bin/tilo-crypto', 'verify-rsa-sha256',
                              str(self.home / 'pcr.pub'), str(source), str(output)])

if __name__ == '__main__':
    unittest.main()
