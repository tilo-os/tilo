#!/usr/bin/env python3
"""Exercise real public-key parsing with disposable, unexported fixture keys."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location('trust', Path(__file__).resolve().parents[1] / 'dev/development-trust.py')
trust = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(trust)
LOCAL_CRYPTO = Path(__file__).resolve().parents[2] / 'target/debug/tilo-crypto'
if LOCAL_CRYPTO.is_file():
    trust.CRYPTO = LOCAL_CRYPTO


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


@unittest.skipUnless(shutil.which('cosign') and trust.CRYPTO.is_file(),
                     'Cosign and Rust-native crypto required')
class PublicBundleTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.scratch = tempfile.TemporaryDirectory(prefix='tilo-trust-fixture-')
        cls.addClassCleanup(cls.scratch.cleanup)
        cls.root = Path(cls.scratch.name)

        private = cls.root / 'fixture-private.pem'
        subprocess.run([trust.CRYPTO, 'generate-secure-boot-cert', 'Tilo disposable test',
                        private, cls.root / trust.FILES[2]], check=True, capture_output=True)
        subprocess.run([trust.CRYPTO, 'rsa-public-key', private,
                        cls.root / trust.FILES[1]], check=True, capture_output=True)
        prefix = cls.root / 'fixture-cosign'
        environment = os.environ.copy()
        environment['COSIGN_PASSWORD'] = ''
        subprocess.run(['cosign', 'generate-key-pair', '--output-key-prefix', prefix],
                       check=True, capture_output=True, env=environment)
        Path(str(prefix) + '.pub').rename(cls.root / trust.FILES[0])
        (cls.root / trust.FILES[3]).write_text('ab' * 32 + '\n')
        cls.sources = [cls.root / name for name in trust.FILES]

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='tilo-public-bundle-')
        self.addCleanup(self.temporary.cleanup)
        self.output = Path(self.temporary.name) / 'public'
        trust.create(self.sources, self.output)

    def replace_public(self, name, data):
        (self.output / name).write_bytes(data)
        path = self.output / 'manifest.json'
        manifest = json.loads(path.read_text())
        manifest['sha256'][name] = hashlib.sha256(data).hexdigest()
        path.write_text(json.dumps(manifest))

    def test_bundle_creation(self):
        with self.subTest(phase="valid_bundle_contains_only_public_inputs"):
            result = trust.validate(self.output)
            self.assertEqual(result['purpose'], 'tilo-development')
            self.assertEqual(set(result['identities']), set(trust.FILES))
            self.assertEqual({p.name for p in self.output.iterdir()}, {*trust.FILES, 'manifest.json'})
            self.assertEqual(result['identities']['registry-owner.pub'], 'ab' * 32)
        restart_fixture(self)
        with self.subTest(phase="existing_output_is_never_overwritten"):
            before = (self.output / 'manifest.json').read_bytes()
            with self.assertRaises(FileExistsError):
                trust.create(self.sources, self.output)
            self.assertEqual((self.output / 'manifest.json').read_bytes(), before)

    def test_digest_binding_rejections(self):
        with self.subTest(phase="tampered_public_key_fails_digest_binding"):
            with (self.output / 'cosign.pub').open('ab') as stream:
                stream.write(b'\n')
            with self.assertRaisesRegex(ValueError, 'digest mismatch'):
                trust.validate(self.output)
        restart_fixture(self)
        with self.subTest(phase="external_digest_rejects_consistent_bundle_substitution"):
            expected = trust.validate(self.output)['manifest_sha256']
            self.assertEqual(trust.validate(self.output, expected)['manifest_sha256'], expected)
            self.replace_public('registry-owner.pub', b'cd' * 32)
            trust.validate(self.output)  # Structurally valid is not identity authentication.
            with self.assertRaisesRegex(ValueError, 'independently supplied digest'):
                trust.validate(self.output, expected)
        restart_fixture(self)
        with self.subTest(phase="private_key_rejected_even_with_updated_manifest"):
            private = (self.root / 'fixture-private.pem').read_bytes()
            for data in (private, self.sources[0].read_bytes() + private):
                with self.subTest(appended=data != private):
                    self.replace_public('cosign.pub', data)
                    with self.assertRaisesRegex(ValueError, 'exactly one public'):
                        trust.validate(self.output)
        restart_fixture(self)
        with self.subTest(phase="wrong_pcr_algorithm_rejected"):
            self.replace_public(trust.FILES[1], self.sources[0].read_bytes())
            with self.assertRaises(ValueError):
                trust.validate(self.output)
        restart_fixture(self)
        with self.subTest(phase="non_development_manifest_rejected"):
            path = self.output / 'manifest.json'
            manifest = json.loads(path.read_text())
            manifest['purpose'] = 'production'
            path.write_text(json.dumps(manifest))
            with self.assertRaisesRegex(ValueError, 'invalid development'):
                trust.validate(self.output)
        restart_fixture(self)
        with self.subTest(phase="duplicate_manifest_properties_rejected"):
            path = self.output / 'manifest.json'
            path.write_text(path.read_text().replace('"schema": 1', '"schema": 1, "schema": 1'))
            with self.assertRaisesRegex(ValueError, 'duplicate'):
                trust.validate(self.output)

    def test_filesystem_rejections(self):
        with self.subTest(phase="extra_file_rejected"):
            (self.output / 'unexpected.pem').write_text('unexpected')
            with self.assertRaisesRegex(ValueError, 'unexpected or missing'):
                trust.validate(self.output)
        restart_fixture(self)
        with self.subTest(phase="symlink_rejected"):
            path = self.output / 'cosign.pub'
            path.unlink()
            try:
                path.symlink_to(self.sources[0])
            except OSError:
                self.skipTest('symlink creation unavailable')
            with self.assertRaisesRegex(ValueError, 'link or junction'):
                trust.validate(self.output)
        restart_fixture(self)
        with self.subTest(phase="redirected_parent_rejected"):
            parent = self.output.parent / 'redirect'
            try:
                parent.symlink_to(self.output.parent, target_is_directory=True)
            except OSError:
                self.skipTest('symlink creation unavailable')
            with self.assertRaisesRegex(ValueError, 'link or junction'):
                trust.validate(parent / self.output.name)
        restart_fixture(self)
        with self.subTest(phase="zero_owner_and_oversized_input_rejected"):
            self.replace_public('registry-owner.pub', b'0' * 64)
            with self.assertRaisesRegex(ValueError, 'nonzero'):
                trust.validate(self.output)
            self.replace_public('registry-owner.pub', b'a' * (trust.LIMIT + 1))
            with self.assertRaisesRegex(ValueError, 'bounded regular'):
                trust.validate(self.output)


if __name__ == '__main__':
    unittest.main()
