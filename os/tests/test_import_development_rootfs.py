"""Importer boundary tests; registry/native image behavior needs the CI fixture."""
import copy
from contextlib import contextmanager
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import test_rebind_development_trust as fixtures

spec = importlib.util.spec_from_file_location('importer', fixtures.DEV / 'import-development-rootfs.py')
importer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(importer)


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


@unittest.skipUnless(hasattr(os, 'getuid') and os.getuid() == 0, 'native root fixture required')
class ImportTests(unittest.TestCase):
    setUpClass = fixtures.RebindTests.__dict__['setUpClass']

    def setUp(self):
        fixtures.RebindTests.setUp(self)
        self.addCleanup(self.temporary.cleanup)
        self.image_id = 'sha256:' + 'c' * 64
        self.revision = 'a' * 40
        self.manifest = json.dumps({'schemaVersion': 2,
            'mediaType': 'application/vnd.oci.image.manifest.v1+json',
            'config': {'digest': self.image_id, 'mediaType': 'application/vnd.oci.image.config.v1+json'}})
        digest = 'sha256:' + hashlib.sha256(self.manifest.encode()).hexdigest()
        self.reference = importer.retention.REPOSITORY + '@' + digest
        handoff = dict(schema=1, purpose='tilo-development-raw-rootfs-handoff', revision=self.revision,
            architecture=platform.machine(), public_manifest_sha256=self.arguments['old_manifest'],
            target_update_ref=self.target, generation=7, source_date_epoch=0, installable=False,
            sealed=False, published=False, raw_rootfs_id=self.image_id,
            validation='captured build inputs only; independently revalidate before reuse')
        self.receipt = dict(schema=1, purpose='tilo-retained-development-raw-rootfs',
            image=self.reference, handoff=handoff, handoff_sha256=importer.retention.receipt_digest(handoff),
            hardware_profile=None, published=True, installable=False, sealed=False, kernel_version='test',
            layer_private_material_audit='not performed by this helper; required before publication',
            completion_required=['rebind public trust and origin', 'regenerate initramfs',
                                 'sign loader', 'seal and sign UKIs', 'disk and VM verification'])
        self.path = Path(self.temporary.name) / 'retained.json'
        self.args = SimpleNamespace(receipt=self.path, public_bundle=self.old,
            revision=self.revision, architecture=platform.machine(), manifest_sha256=self.arguments['old_manifest'],
            target=self.target, generation=7, source_date_epoch=0)
        self.signature = json.dumps([{'critical': {'type': 'cosign container image signature',
            'image': {'docker-manifest-digest': digest},
            'identity': {'docker-reference': importer.retention.REPOSITORY}},
            'optional': {importer.retention.ANNOTATION: self.receipt['handoff_sha256']}}])
        self.info = json.dumps([{'Id': self.image_id, 'Os': 'linux',
            'Architecture': importer.source.ARCHITECTURES[platform.machine()],
            'Digest': digest, 'RepoDigests': [self.reference],
            'Labels': {'org.opencontainers.image.revision': self.revision}}])

    def execute(self):
        self.path.write_text(json.dumps(self.receipt))
        @contextmanager
        def mounted(image_id):
            self.assertEqual(image_id, self.image_id)
            try:
                yield self.image
            finally:
                self.unmounted = True
        self.unmounted = False
        with patch.dict(os.environ, {}, clear=True), \
             patch.object(importer.source, 'command', side_effect=[self.manifest, self.signature, '']) as remote, \
             patch.object(importer.rootfs, 'command', return_value=self.info), \
             patch.object(importer.rootfs, 'mounted', side_effect=mounted):
            try:
                with importer.verified_rootfs(self.args) as verified:
                    self.assertEqual(verified.image_id, self.image_id)
                    self.assertFalse(verified.receipt['installable'])
                    self.assertTrue(verified.public_bundle.is_dir())
            finally:
                self.commands = [call.args[0] for call in remote.call_args_list]
                self.environments = [call.args[1] for call in remote.call_args_list]

    def test_pulled_source_validation(self):
        with self.subTest(phase="valid_source_is_held_then_unmounted_without_image_execution"):
            before = fixtures.RebindTests.snapshot(self)
            self.execute()
            self.assertTrue(self.unmounted)
            self.assertEqual(fixtures.RebindTests.snapshot(self), before)
            self.assertEqual([cmd[1] for cmd in self.commands], ['inspect', 'verify', 'pull'])
            self.assertIn('--tls-verify=true', self.commands[0])
            self.assertIn('--tls-verify=true', self.commands[2])
            self.assertEqual(self.environments[1]['PATH'], '/usr/local/bin:/usr/bin:/bin')
        restart_fixture(self)
        with self.subTest(phase="pulled_config_architecture_revision_and_origin_are_revalidated"):
            original = json.loads(self.info)[0]
            for field, value in [('Id', 'sha256:' + 'd' * 64), ('Architecture', 'unsupported'),
                                 ('Labels', {'org.opencontainers.image.revision': 'd' * 40})]:
                self.info = json.dumps([original | {field: value}])
                with self.subTest(field=field), self.assertRaises(ValueError):
                    self.execute()
                self.assertFalse(self.unmounted)
            self.info = json.dumps([original])
            # Installed trust policy must be revalidated, independently of registry labels.
            policy = self.image / 'etc/containers/policy.json'
            policy.write_text('{"default":[{"type":"insecureAcceptAnything"}]}')
            with self.assertRaises(ValueError):
                self.execute()
            self.assertTrue(self.unmounted)
        restart_fixture(self)
        with self.subTest(phase="signed_sealed_pending_and_wrong_generation_fail_with_unmount"):
            for relative in ('boot/EFI/Linux', 'kernel', 'usr/lib/systemd/boot/efi/test.signed',
                             str(importer.hardware.DEVELOPMENT_PATH / 'initramfs-rebind-required.json'),
                             'usr/lib/tilo/generation'):
                path = self.image / relative
                old = path.read_bytes() if path.exists() else None
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b'wrong')
                with self.subTest(path=relative), self.assertRaises(ValueError):
                    self.execute()
                self.assertTrue(self.unmounted)
                if old is None:
                    path.unlink()
                else:
                    path.write_bytes(old)

    def test_pre_network_rejections(self):
        with self.subTest(phase="bad_digest_repository_and_canonical_receipt_reject_before_network"):
            original = copy.deepcopy(self.receipt)
            for field, value in [('image', self.reference.replace('rootfs@', 'rootfs-other@')),
                                 ('image', importer.retention.REPOSITORY + ':tag'),
                                 ('handoff_sha256', '0' * 64), ('sealed', True)]:
                self.receipt = original | {field: value}
                with self.subTest(field=field), self.assertRaises(ValueError):
                    self.execute()
                self.assertEqual(self.commands, [])
        restart_fixture(self)
        with self.subTest(phase="independent_epoch_and_origin_reject_before_network"):
            for field, value in [('source_date_epoch', 1), ('target', self.target + '-wrong')]:
                original = getattr(self.args, field)
                setattr(self.args, field, value)
                with self.subTest(field=field), self.assertRaises(ValueError):
                    self.execute()
                self.assertEqual(self.commands, [])
                setattr(self.args, field, original)
        restart_fixture(self)
        with self.subTest(phase="wrong_signature_annotation_rejects_before_pull"):
            self.signature = self.signature.replace(self.receipt['handoff_sha256'], 'f' * 64)
            with self.assertRaises(ValueError):
                self.execute()
            self.assertEqual(len(self.commands), 2)
        restart_fixture(self)
        with self.subTest(phase="wrong_manifest_bytes_or_config_rejects_before_signature_or_pull"):
            original = self.manifest
            for kind in ('digest', 'config'):
                self.manifest = original + ' ' if kind == 'digest' else original.replace('c' * 64, 'd' * 64)
                if kind == 'config':
                    self.receipt['image'] = importer.retention.REPOSITORY + '@sha256:' + hashlib.sha256(self.manifest.encode()).hexdigest()
                with self.subTest(kind=kind), self.assertRaises(ValueError):
                    self.execute()
                self.assertEqual(len(self.commands), 1)
        restart_fixture(self)
        with self.subTest(phase="redirected_original_bundle_rejects_before_network"):
            alias = Path(self.temporary.name) / 'alias'
            alias.symlink_to(self.old, target_is_directory=True)
            self.args.public_bundle = alias
            with self.assertRaises(ValueError):
                self.execute()
            self.assertEqual(self.commands, [])


if __name__ == '__main__':
    unittest.main()
