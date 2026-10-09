"""Exercise the real broker client with public-only synthetic RPM responses."""
import importlib.util
import json
from pathlib import Path
import struct
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('rpm_broker', ROOT / 'dist/rpm/sign-rpms-broker.py')
CLIENT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CLIENT)
PIN = 'A' * 40


def rpm_bytes(signature=b'', payload=b'original payload'):
    lead = bytes.fromhex('edabeedb') + bytes(92)
    header = CLIENT.MAGIC + bytes(4) + struct.pack('>II', 0, len(signature)) + signature
    header += bytes((-len(header)) % 8)
    return lead + header + CLIENT.MAGIC + payload


class RpmBrokerTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.input = self.root / 'input'
        self.input.mkdir()
        for name in ('tilo-session', 'tilo-session-apps', 'tilo-session-devel'):
            (self.input / f'{name}-0.1.13-1.fc44.x86_64.rpm').write_bytes(rpm_bytes())
        self.env = {'GITHUB_REPOSITORY': 'tilo/tilo', 'GITHUB_EVENT_NAME': 'push',
                    'GITHUB_REF': 'refs/tags/desktop-v0.1.13', 'GITHUB_SHA': 'b' * 40,
                    'GITHUB_RUN_ID': '123', 'GITHUB_RUN_ATTEMPT': '2',
                    'TILO_RPM_ARCH': 'amd64', 'TILO_RPM_SIGNING_FINGERPRINT': PIN,
                    'ACTIONS_ID_TOKEN_REQUEST_URL': 'not-fetched-in-this-test',
                    'ACTIONS_ID_TOKEN_REQUEST_TOKEN': 'not-logged',
                    'GITHUB_WORKSPACE': str(ROOT), 'RUNNER_TEMP': str(self.root)}
        self.request = CLIENT.request_for(self.input, self.env)
        self.request_path = self.root / 'request.json'
        self.request_path.write_text(json.dumps(self.request))
        self.response = self.root / 'response'
        self.response.mkdir()
        for file in self.input.glob('*.rpm'):
            (self.response / file.name).write_bytes(rpm_bytes(b'synthetic signature'))
        (self.response / 'tilo-rpm-signing-key.asc').write_text('-----BEGIN PGP PUBLIC KEY BLOCK-----\npublic fixture\n')
        self.receipt = {'schema': 1, 'request_sha256': CLIENT.digest(self.request_path),
                        'fingerprint': PIN, 'signed_sha256': {f.name: CLIENT.digest(f) for f in self.response.glob('*.rpm')}}
        self.save_receipt()

    def save_receipt(self):
        (self.response / 'receipt.json').write_text(json.dumps(self.receipt))

    def qualify(self, listing=None, signature=None):
        listing = listing or f'pub:::::::::\nfpr:::::::::{PIN}:\n'
        signature = signature or f'Header V4 RSA/SHA256 Signature, key ID {PIN[-16:]}: OK\n'
        with patch.object(CLIENT, 'run', side_effect=[listing, '', signature, signature, signature]):
            return CLIENT.verify_response(self.request, self.request_path, self.response, self.root)

    def test_signed_copies_retain_exact_immutable_header_and_payload(self):
        self.assertEqual(self.qualify(), self.receipt)
        self.assertNotIn('not-logged', self.request_path.read_text())
        self.assertNotIn('ACTIONS_ID_TOKEN', self.request_path.read_text())

    def test_immutable_hash_allows_signature_change_but_detects_payload_change(self):
        file = next(self.response.glob('*.rpm'))
        original = next(self.input.glob('*.rpm'))
        self.assertEqual(CLIENT.immutable_digest(file), CLIENT.immutable_digest(original))
        file.write_bytes(rpm_bytes(b'signed', b'substituted payload'))
        self.assertNotEqual(CLIENT.immutable_digest(file), CLIENT.immutable_digest(original))
        self.receipt['signed_sha256'][file.name] = CLIENT.digest(file)
        self.save_receipt()
        with self.assertRaisesRegex(ValueError, 'changed package'):
            self.qualify()

    def test_receipt_from_another_run_is_rejected(self):
        self.receipt['request_sha256'] = '0' * 64
        self.save_receipt()
        with self.assertRaisesRegex(ValueError, 'another request'):
            self.qualify()

    def test_independent_pin_is_required(self):
        self.receipt['fingerprint'] = 'B' * 40
        self.save_receipt()
        with self.assertRaisesRegex(ValueError, 'protected pin'):
            self.qualify()

    def test_extra_primary_key_is_rejected(self):
        listing = f'pub:::::::::\nfpr:::::::::{PIN}:\npub:::::::::\nfpr:::::::::{"B" * 40}:\n'
        with self.assertRaisesRegex(ValueError, 'extra primary'):
            self.qualify(listing=listing)

    def test_private_key_and_extra_output_are_rejected(self):
        key = self.response / 'tilo-rpm-signing-key.asc'
        key.write_text('-----BEGIN PGP PRIVATE KEY BLOCK-----\n')
        with self.assertRaisesRegex(ValueError, 'only an armored public key'):
            self.qualify()
        (self.response / 'unrequested').write_text('not an artifact')
        with self.assertRaisesRegex(ValueError, 'unexpected or missing'):
            self.qualify()

    def test_signed_output_symlinks_are_rejected(self):
        file = next(self.response.glob('*.rpm'))
        file.unlink()
        file.symlink_to(next(self.input.glob('*.rpm')))
        with self.assertRaisesRegex(ValueError, 'not a regular'):
            self.qualify()

    def test_unsigned_wrong_key_and_negative_verification_are_rejected(self):
        for output in ('Header SHA256 digest: OK\n', 'Signature, key ID deadbeef: OK\n',
                       f'Signature, key ID {PIN[-16:]}: OK\nPayload: BAD\n'):
            with self.subTest(output=output), self.assertRaises(ValueError):
                self.qualify(signature=output)
            # Each verification allocates a fresh public-only keyring.
            for name in ('gnupg', 'rpmdb'):
                directory = self.root / name
                if directory.exists():
                    directory.rmdir()

    def test_artifact_digest_tampering_is_rejected(self):
        file = next(self.response.glob('*.rpm'))
        file.write_bytes(rpm_bytes(b'different signature'))
        with self.assertRaisesRegex(ValueError, 'digest mismatch'):
            self.qualify()

    def test_unprotected_context_private_credentials_and_wrong_arch_rejected(self):
        for name, value in (('GITHUB_REF', 'refs/heads/main'), ('GITHUB_EVENT_NAME', 'workflow_dispatch'),
                            ('GITHUB_REPOSITORY', 'fork/tilo'), ('TILO_RPM_SIGNING_KEY_B64', 'forbidden'),
                            ('TILO_RPM_ARCH', 'arm64'), ('GITHUB_RUN_ATTEMPT', '0')):
            with self.subTest(name=name), self.assertRaises(ValueError):
                CLIENT.request_for(self.input, self.env | {name: value})

    def test_untrusted_broker_rejected_before_any_oidc_request(self):
        with self.assertRaises(ValueError):
            CLIENT.trusted_broker(ROOT / 'dist/rpm/sign-rpms.sh', self.env)
        with self.assertRaises(ValueError):
            CLIENT.trusted_broker(Path('relative'), self.env)

    def test_malformed_rpm_signature_header_fails_closed(self):
        file = next(self.input.glob('*.rpm'))
        for data in (b'not an rpm', rpm_bytes()[:100],
                     bytes.fromhex('edabeedb') + bytes(92) + CLIENT.MAGIC + bytes(4) + struct.pack('>II', 4097, 0)):
            file.write_bytes(data)
            with self.assertRaises(ValueError):
                CLIENT.immutable_digest(file)

    def test_failed_qualification_never_replaces_install_tested_rpms(self):
        before = {f.name: f.read_bytes() for f in self.input.glob('*.rpm')}
        with patch.object(CLIENT, 'trusted_broker'), patch.object(CLIENT.subprocess, 'run'), \
                patch.object(CLIENT, 'verify_response', side_effect=ValueError('invalid signed response')):
            with self.assertRaises(ValueError):
                CLIENT.sign(self.input, self.env)
        self.assertEqual({f.name: f.read_bytes() for f in self.input.glob('*.rpm')}, before)
        self.assertFalse(list(self.input.glob('*.asc')))

    def test_workflow_keeps_main_session_gate_and_isolates_oidc_from_builds(self):
        workflow = (ROOT / '.forgejo/workflows/release-desktop.yml').read_text()
        package = workflow.split('  package:\n')[1].split('  sign-rpm:\n')[0]
        signer = workflow.split('  sign-rpm:\n')[1].split('  host-session-e2e:\n')[0]
        self.assertNotIn('TILO_RPM_SIGNING_KEY_', workflow)
        self.assertNotIn('enable-openid-connect', package)
        self.assertIn('enable-openid-connect: true', signer)
        self.assertIn('needs: [validate, package]', signer)
        self.assertIn("needs.package.result == 'success'", workflow)
        self.assertIn("!startsWith(github.ref, 'refs/tags/desktop-v')", workflow)
        self.assertIn('needs: [validate, package, sign-rpm, host-session-e2e]', workflow)
        self.assertIn("'signed-fedora-44-amd64' || 'fedora-44-amd64'", workflow)
        build = (ROOT / 'dist/ci/run-package-job.sh').read_text()
        self.assertNotIn('--env TILO_RPM_SIGNING', build)
        self.assertNotIn('dist/rpm/sign-rpms.sh', build)


if __name__ == '__main__':
    unittest.main()
