import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location(
    'channel', Path(__file__).parents[1] / 'apps/app_channel_manifest.py'
)
c = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(c)


def encode(value):
    return json.dumps(value, sort_keys=True).encode()


class ChannelReaderTests(unittest.TestCase):
    def setUp(self):
        self.policy = {'app_publishers': ['1' * 64], 'runtime_publisher': '2' * 64,
                       'verifier_sha256': '3' * 64, 'allowed_package_isa': {'x86_64': ['v3'], 'aarch64': ['armv8-a']}}
        self.receipt = {'schema': c.RECEIPT_SCHEMA, 'architecture': 'x86_64', 'source_sha': 'a' * 40,
                        'collection': {'format': 'tilo-apps-tar-gzip/v1', 'sha256': '4' * 64, 'bytes': 123},
                        'runtime': {'version': '2026.07', 'root_hash': '5' * 64, 'publisher': '2' * 64, 'signature_hex': '6' * 128},
                        'packages': [{'file': 'app-1-x86_64.tapp', 'app_id': 'dev.test.app', 'version': '1', 'sha256': '7' * 64,
                                      'package_id': 'tapp:' + '8' * 64, 'publisher': '1' * 64, 'isa': 'v3'}],
                        'verification': {'verifier_sha256': '3' * 64, 'package_identity': 'blake3-ed25519-signed-root-v1',
                                         'runtime_identity': 'blake3-ed25519-runtime-bundle-v1'},
                        'publication': {'status': 'unpublished'}}
        self.upload = {'schema': 'tilo.app-upload/v1', 'source_sha': 'a' * 40, 'architecture': 'x86_64',
                       'sha256': '4' * 64, 'bytes': 123,
                       'url': 'https://code.tilo.so/api/packages/tilo/generic/tilo-app-collections/0.0.0-' + 'a' * 40 + '/tilo-apps-x86_64.tar.gz'}

    def published(self):
        raw = encode(self.upload)
        return c.attach_publication(self.receipt, raw, hashlib.sha256(raw).hexdigest(), self.policy)

    def manifest(self, receipt):
        # A channel can advance other architectures beyond this preserved x86 entry.
        return encode({'schema': c.CHANNEL_SCHEMA, 'generation': 3, 'source_sha': 'b' * 40,
                       'architectures': {'x86_64': receipt}})

    def select(self, raw, architecture='x86_64', policy=None, **kwargs):
        return c.select(raw, hashlib.sha256(raw).hexdigest(), 'c' * 40, architecture, policy or self.policy, **kwargs)

    def test_channel_reader_consumes_only_published_exact_architecture_entries(self):
        with self.subTest(phase='exact architecture selection preserves entry source'):
            self.doCleanups()
            self.setUp()
            result = self.select(self.manifest(self.published()))
            self.assertEqual(result['source_sha'], 'a' * 40)
            self.assertEqual(result['channel_commit'], 'c' * 40)
            self.assertEqual(result['runtime']['root_hash'], '5' * 64)
            self.assertTrue(result['usable_for_download'])
            with self.assertRaisesRegex(ValueError, 'fallback forbidden'):
                self.select(self.manifest(self.published()), 'aarch64')
        with self.subTest(phase='candidate cannot be consumed as published'):
            self.doCleanups()
            self.setUp()
            raw = self.manifest(self.receipt)
            with self.assertRaisesRegex(ValueError, 'unpublished collection'):
                self.select(raw)
            self.assertFalse(self.select(raw, require_published=False)['usable_for_download'])
        with self.subTest(phase='audit schema and missing signed isa are never consumable'):
            self.doCleanups()
            self.setUp()
            receipt = self.published()
            receipt['schema'] = 'tilo.app-collection-audit/v1'
            with self.assertRaisesRegex(ValueError, 'architecture/schema mismatch'):
                self.select(self.manifest(receipt))
            receipt['schema'] = c.RECEIPT_SCHEMA
            receipt['packages'][0]['isa'] = None
            with self.assertRaisesRegex(ValueError, 'trust/ISA mismatch'):
                self.select(self.manifest(receipt))
        with self.subTest(phase='manifest digest tamper and duplicate keys fail closed'):
            self.doCleanups()
            self.setUp()
            raw = self.manifest(self.published())
            with self.assertRaisesRegex(ValueError, 'trusted digest mismatch'):
                c.select(raw + b' ', hashlib.sha256(raw).hexdigest(), 'c' * 40, 'x86_64', self.policy)
            with self.assertRaisesRegex(ValueError, 'duplicate JSON key'):
                c.read_json(b'{"source_sha":"a","source_sha":"b"}')

    def test_channel_trust_requires_pinned_policy_and_matching_signed_identities(self):
        with self.subTest(phase='external publisher verifier and isa policy required'):
            self.doCleanups()
            self.setUp()
            raw = self.manifest(self.published())
            for key, value in [('app_publishers', ['0' * 64]), ('runtime_publisher', '0' * 64),
                               ('verifier_sha256', '0' * 64), ('allowed_package_isa', {'x86_64': ['v2']})]:
                with self.subTest(key=key), self.assertRaises(ValueError):
                    self.select(raw, policy=dict(self.policy, **{key: value}))
        with self.subTest(phase='canonical policy requires matching backend and binary identities'):
            self.doCleanups()
            self.setUp()
            hashes = {'tilo_pkg_sha256': '7' * 64, 'tilo_runtime_bundle_sha256': '8' * 64}
            policy = dict(self.policy, canonical_binaries=hashes)
            receipt = self.published()
            with self.assertRaises(ValueError):
                self.select(self.manifest(receipt), policy=policy)
            receipt['verification'].update(backend='canonical-cli-v1', canonical_binaries=hashes)
            self.assertTrue(self.select(self.manifest(receipt), policy=policy)['usable_for_download'])
            receipt['verification']['canonical_binaries'] = dict(hashes, tilo_pkg_sha256='9' * 64)
            with self.assertRaises(ValueError):
                self.select(self.manifest(receipt), policy=policy)
        with self.subTest(phase='uploader receipt must match archive and trusted digest'):
            self.doCleanups()
            self.setUp()
            raw = encode(dict(self.upload, sha256='0' * 64))
            with self.assertRaisesRegex(ValueError, 'does not match verified archive'):
                c.attach_publication(self.receipt, raw, hashlib.sha256(raw).hexdigest(), self.policy)
            with self.assertRaisesRegex(ValueError, 'trusted digest mismatch'):
                c.attach_publication(self.receipt, encode(self.upload), '0' * 64, self.policy)
        with self.subTest(phase='mutable url and cross architecture receipts rejected'):
            self.doCleanups()
            self.setUp()
            for url in (self.upload['url'].replace('0.0.0-' + 'a' * 40, 'latest'),
                        self.upload['url'].replace('code.tilo.so', 'example.invalid')):
                raw = encode(dict(self.upload, url=url))
                with self.assertRaises(ValueError):
                    c.attach_publication(self.receipt, raw, hashlib.sha256(raw).hexdigest(), self.policy)
            receipt = self.published()
            receipt['architecture'] = 'aarch64'
            with self.assertRaisesRegex(ValueError, 'architecture/schema mismatch'):
                self.select(self.manifest(receipt))
        with self.subTest(phase='malformed signed identity rejected'):
            self.doCleanups()
            self.setUp()
            receipt = self.published()
            receipt['packages'][0]['package_id'] = 'tapp:unsigned'
            with self.assertRaisesRegex(ValueError, 'invalid digest'):
                self.select(self.manifest(receipt))
        with self.subTest(phase='adapter rejects unpinned verifier before import'):
            self.doCleanups()
            self.setUp()
            with tempfile.TemporaryDirectory() as d:
                root = Path(d)
                archive = root / 'archive'
                archive.write_bytes(b'archive')
                verifier = root / 'verifier.py'
                verifier.write_text('raise AssertionError("must not execute")')
                with self.assertRaisesRegex(ValueError, 'trusted verifier digest mismatch'):
                    c.create_receipt(archive, root, c.sha(archive), 'a' * 40, 'x86_64', self.policy, verifier, '0' * 64)


if __name__ == '__main__':
    unittest.main()
