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


class SigningSpoolTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        # Test the actual signer implementation; do not replace its backend.
        # Run in the prepared native tool container, never install test tools
        # globally or substitute the older OpenSSL signer on newer main.
        required = ['/usr/bin/openssl', '/usr/bin/sbverify', '/usr/bin/unshare', '/usr/bin/mount']
        if '/usr/local/bin/tilo-crypto' in spool.signer.sign.__code__.co_consts:
            required.append('/usr/local/bin/tilo-crypto')
        else:
            required.append('/usr/bin/sbsign')
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
        for key in ('pcr-signing.key', 'efi-signing.key'):
            spool.signer.run(['/usr/bin/openssl', 'genpkey', '-algorithm', 'RSA', '-pkeyopt', 'rsa_keygen_bits:2048', '-out', str(cls.home / key)])
            (cls.home / key).chmod(0o600)
        spool.signer.run(['/usr/bin/openssl', 'pkey', '-in', str(cls.home / 'pcr-signing.key'), '-pubout', '-out', str(cls.home / 'pcr.pub')])
        spool.signer.run(['/usr/bin/openssl', 'req', '-new', '-x509', '-key', str(cls.home / 'efi-signing.key'), '-subj', '/CN=Throwaway test/', '-days', '1', '-out', str(cls.home / 'efi-signing.crt')])

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def setUp(self):
        self.work = Path(tempfile.mkdtemp(dir=self.root))
        for name in ('spool', 'state', 'out'):
            (self.work / name).mkdir(mode=0o700)
        self.context = {'schema': 1, 'job_id': 'fixture-job', 'pod_uid': 'fixture-pod', 'container_id': 'fixture-container',
                        'image_id': 'sha256:' + 'a' * 64, 'revision': 'a' * 40, 'architecture': 'aarch64',
                        'manifest_sha256': 'b' * 64, 'target': 'registry.invalid/qos/test:aarch64'}
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

    def test_real_rsa_remote_adapter_roundtrip_and_replay_rejection(self):
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
        spool.signer.run(['/usr/bin/openssl', 'dgst', '-sha256', '-verify', str(self.home / 'pcr.pub'), '-signature', str(destination), str(source)])
        with self.assertRaises(FileExistsError):
            self.service(requests[0].parent, policy)
        with self.assertRaisesRegex(ValueError, 'already consumed'):
            self.service(self.request(data, '2' * 32), policy)

    def test_actual_efi_response_verified_with_fixture_certificate(self):
        data = synthetic_uki('aarch64')
        request = self.request(data, operation='sign-efi')
        self.service(request, self.policy(data, 'uki', 'sign-efi'))
        spool.signer.run(['/usr/bin/sbverify', '--cert', str(self.home / 'efi-signing.crt'), str(request / 'output')])

    def test_mismatched_input_and_context_never_reserve_authorization(self):
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

    def test_unapproved_bytes_symlink_and_size_fail_closed(self):
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

    def test_publication_is_single_link_before_directory_sync_and_never_overwrites(self):
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

    def test_reservation_is_fsynced_before_private_key_use(self):
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

    def test_readonly_signer_home_real_private_mount_namespace(self):
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
        spool.signer.run(['/usr/bin/openssl', 'dgst', '-sha256', '-verify', str(self.home / 'pcr.pub'),
                         '-signature', str(output), str(source)])

    def test_cross_request_response_rejected_without_output(self):
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

    def test_authorization_checks_exact_hash_architecture_and_seal(self):
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

if __name__ == '__main__':
    unittest.main()

