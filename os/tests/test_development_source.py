#!/usr/bin/env python3
"""Fail-closed identity checks for the independent development source gate."""
import importlib.util
import json
from pathlib import Path
import sys
import unittest
from unittest import mock
from types import SimpleNamespace
import tempfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'dev'))
spec = importlib.util.spec_from_file_location('development_source', Path(__file__).resolve().parents[1] / 'dev/verify-development-source.py')
source = importlib.util.module_from_spec(spec)
spec.loader.exec_module(source)


class IdentityTests(unittest.TestCase):
    repository = '127.0.0.1:5000/tilo-development/os'
    digest = 'sha256:' + 'a' * 64
    revision = 'b' * 40

    def setUp(self):
        self.reference = self.repository + '@' + self.digest
        self.payload = [{'critical': {'type': 'cosign container image signature',
                                     'image': {'docker-manifest-digest': self.digest},
                                     'identity': {'docker-reference': self.repository}}}]
        self.image = [{'Os': 'linux', 'Architecture': 'arm64', 'Digest': self.digest,
                       'RepoDigests': [self.reference], 'Id': 'c' * 64,
                       'Labels': {'org.opencontainers.image.revision': self.revision},
                       'RootFS': {'Layers': ['sha256:' + 'e' * 64]}}]

    def test_pending_rebind_marker_rejects_file_directory_and_dangling_symlink(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            marker = root / source.hardware.DEVELOPMENT_PATH / 'initramfs-rebind-required.json'
            marker.parent.mkdir(parents=True)
            for kind in ('file', 'directory', 'symlink'):
                if kind == 'file':
                    marker.write_text('{}')
                elif kind == 'directory':
                    marker.mkdir()
                else:
                    marker.symlink_to(root / 'missing')
                with self.subTest(kind=kind), self.assertRaisesRegex(ValueError, 'verified initramfs regeneration'):
                    source.mounted_identity(root, 'unused', 'unused', 'aarch64')
                marker.rmdir() if kind == 'directory' else marker.unlink()

    def test_immutable_source_and_mutable_update_tag_share_repository(self):
        source.inputs(self.reference, self.repository + ':test', 'aarch64', self.revision)

    def test_retained_registry_target_matches_native_source_architecture(self):
        repository = 'code.sandbox.example/qos/tilo-development'
        for architecture in ('aarch64', 'x86_64'):
            source.inputs(repository + '@' + self.digest, repository + ':development-' + architecture,
                          architecture, self.revision)
        for tag in ('development-x86_64', 'latest', 'development-aarch64-other'):
            with self.subTest(tag=tag), self.assertRaises(ValueError):
                source.inputs(repository + '@' + self.digest, repository + ':' + tag,
                              'aarch64', self.revision)

    def test_reject_wrong_repository_mutable_source_and_production(self):
        for ref in (self.repository + ':test', self.reference.replace('5000', '5001'),
                    'code.sandbox.example/qos/tilo@' + self.digest):
            with self.subTest(ref=ref), self.assertRaises(ValueError):
                source.inputs(ref, self.repository + ':test', 'aarch64', self.revision)

    def test_exact_signed_identity(self):
        source.signature_payloads(json.dumps(self.payload), self.reference)
        for field, key in (('image', 'docker-manifest-digest'), ('identity', 'docker-reference')):
            payload = json.loads(json.dumps(self.payload))
            payload[0]['critical'][field][key] += 'wrong'
            with self.subTest(field=field), self.assertRaises(ValueError):
                source.signature_payloads(json.dumps(payload), self.reference)

    def test_empty_duplicate_and_mixed_payloads_rejected(self):
        for raw in ('[]', '{}', '[{"critical":{},"critical":{}}]', json.dumps(self.payload + [{'critical': {}}])):
            with self.subTest(raw=raw), self.assertRaises(ValueError):
                source.signature_payloads(raw, self.reference)

    def test_native_architecture_and_revision_are_bound(self):
        self.assertEqual(source.image_identity(json.dumps(self.image), self.reference, 'aarch64', self.revision), 'c' * 64)
        self.assertEqual(source.oci_evidence(json.dumps(self.image), self.reference), {
            'manifest_digest': self.digest, 'config_digest': 'sha256:' + 'c' * 64,
            'layer_digests': ['sha256:' + 'e' * 64]})
        for key, value in (('Architecture', 'amd64'), ('Os', 'windows'), ('Digest', 'sha256:' + 'd' * 64),
                           ('RepoDigests', []), ('Labels', {}), ('Id', 'mutable-tag')):
            image = json.loads(json.dumps(self.image))
            image[0][key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                source.image_identity(json.dumps(image), self.reference, 'aarch64', self.revision)

    def test_unsigned_source_never_reaches_podman(self):
        with tempfile.TemporaryDirectory() as directory:
            bundle = Path(directory) / 'bundle'
            bundle.mkdir()
            args = SimpleNamespace(source=self.reference, target=self.repository + ':test',
                                   architecture='aarch64', revision=self.revision,
                                   public_bundle=bundle, manifest_sha256='d' * 64,
                                   offline_key_signature=False)
            with mock.patch.object(source.os, 'getuid', return_value=0), \
                    mock.patch.dict(source.os.environ, {}, clear=True), \
                    mock.patch.object(source.trust, 'validate'), \
                    mock.patch.object(source, 'command', side_effect=ValueError('signature rejected')) as run:
                with self.assertRaisesRegex(ValueError, 'signature rejected'):
                    source.verify(args)
            self.assertEqual(len(run.call_args_list), 1)
            self.assertEqual(run.call_args.args[0][:2], ['cosign', 'verify'])

    def test_image_mount_is_released_when_embedded_trust_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            bundle = Path(directory) / 'bundle'
            bundle.mkdir()
            args = SimpleNamespace(source=self.reference, target=self.repository + ':test',
                                   architecture='aarch64', revision=self.revision,
                                   public_bundle=bundle, manifest_sha256='d' * 64,
                                   offline_key_signature=True)
            with mock.patch.object(source.os, 'getuid', return_value=0), \
                    mock.patch.dict(source.os.environ, {}, clear=True), \
                    mock.patch.object(source.trust, 'validate'), \
                    mock.patch.object(source, 'mounted_identity', side_effect=ValueError('trust differs')), \
                    mock.patch.object(source, 'command', side_effect=[json.dumps(self.payload), '', json.dumps(self.image), '/image/root', '']) as run:
                with self.assertRaisesRegex(ValueError, 'trust differs'):
                    source.verify(args)
            self.assertEqual(run.call_args.args[0], ['podman', 'image', 'unmount', 'c' * 64])

    def test_consumer_keeps_exact_image_and_public_snapshot_until_cleanup(self):
        for fail in (False, True):
            with self.subTest(consumer_failure=fail), tempfile.TemporaryDirectory() as directory:
                bundle = Path(directory) / 'bundle'
                bundle.mkdir()
                (bundle / 'cosign.pub').write_text('independently pinned public fixture')
                original_auth = Path(directory) / 'auth.json'
                original_auth.write_text('{"auths":{"example":{"auth":"test-secret"}}}')
                args = SimpleNamespace(source=self.reference, target=self.repository + ':test',
                    architecture='aarch64', revision=self.revision, public_bundle=bundle,
                    manifest_sha256='d' * 64, offline_key_signature=True)
                with mock.patch.object(source.os, 'getuid', return_value=0), \
                        mock.patch.dict(source.os.environ, {'REGISTRY_AUTH_FILE': str(original_auth)}, clear=True), \
                        mock.patch.object(source.trust, 'validate'), \
                        mock.patch.object(source, 'mounted_identity', return_value='fixture'), \
                        mock.patch.object(source, 'command', side_effect=[json.dumps(self.payload), '',
                            json.dumps(self.image), '/image/root', '']) as run:
                    try:
                        with source.verified_source(args) as verified:
                            snapshot = verified.public_bundle
                            self.assertTrue(snapshot.is_dir())
                            self.assertEqual(verified.image_id, 'sha256:' + 'c' * 64)
                            self.assertEqual(verified.receipt['image_id'], verified.image_id)
                            self.assertEqual(verified.root, Path('/image/root'))
                            self.assertEqual(len(run.call_args_list), 4)
                            auth_snapshot = verified.auth_file
                            self.assertIsNotNone(auth_snapshot)
                            self.assertEqual(auth_snapshot.read_bytes(), original_auth.read_bytes())
                            for call in run.call_args_list:
                                self.assertEqual(call.args[1]['REGISTRY_AUTH_FILE'], str(auth_snapshot))
                            original_auth.write_text('replaced')
                            self.assertIn('test-secret', auth_snapshot.read_text())
                            self.assertNotIn('test-secret', json.dumps(verified.receipt))
                            # Replacing the caller's bundle or moving its target
                            # tag cannot change the held consumer inputs.
                            (bundle / 'cosign.pub').write_text('replacement')
                            self.assertEqual((snapshot / 'cosign.pub').read_text(),
                                             'independently pinned public fixture')
                            if fail:
                                raise RuntimeError('consumer failed')
                    except RuntimeError as error:
                        self.assertTrue(fail)
                        self.assertEqual(str(error), 'consumer failed')
                    self.assertEqual(run.call_args.args[0],
                                     ['podman', 'image', 'unmount', 'c' * 64])
                    self.assertEqual(len(run.call_args_list), 5)
                    self.assertFalse(snapshot.exists())
                    self.assertFalse(auth_snapshot.exists())


if __name__ == '__main__':
    unittest.main()
