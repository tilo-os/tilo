"""Identity/signature boundaries; native registry publication is a separate gate."""
import copy
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'dev'))
spec = importlib.util.spec_from_file_location('retain', Path(__file__).resolve().parents[1] / 'dev/retain-development-rootfs.py')
retain = importlib.util.module_from_spec(spec)
spec.loader.exec_module(retain)


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


class RetentionTests(unittest.TestCase):
    def setUp(self):
        self.revision, self.manifest = 'a' * 40, 'b' * 64
        self.target = 'registry.tilo.so/tilo/tilo-development:development-x86_64'
        self.receipt = {'schema': 1, 'purpose': 'tilo-development-raw-rootfs-handoff',
                        'revision': self.revision, 'architecture': 'x86_64',
                        'public_manifest_sha256': self.manifest, 'target_update_ref': self.target,
                        'generation': 7, 'installable': False, 'sealed': False, 'published': False,
                        'raw_rootfs_id': 'sha256:' + 'c' * 64, 'source_date_epoch': 123,
                        'validation': 'captured build inputs only; independently revalidate before reuse'}

    def validate(self, receipt):
        return retain.validate_receipt(receipt, self.revision, 'x86_64', self.manifest, self.target, 7, 123)

    def test_handoff_binding(self):
        with self.subTest(phase="valid_handoff_and_canonical_digest"):
            self.assertEqual(self.validate(self.receipt), self.receipt['raw_rootfs_id'])
            self.assertEqual(retain.receipt_digest(self.receipt),
                             retain.receipt_digest(dict(reversed(list(self.receipt.items())))))
        restart_fixture(self)
        with self.subTest(phase="remote_manifest_binds_original_configuration_before_signing"):
            image_id = self.receipt['raw_rootfs_id']
            manifest = {'schemaVersion': 2, 'mediaType': 'application/vnd.oci.image.manifest.v1+json',
                        'config': {'digest': image_id, 'mediaType': 'application/vnd.oci.image.config.v1+json'}}
            retain.verify_manifest(json.dumps(manifest), image_id)
            for changed in (manifest | {'config': manifest['config'] | {'digest': 'sha256:' + 'd' * 64}},
                            manifest | {'mediaType': 'application/vnd.oci.image.index.v1+json'},
                            manifest | {'config': None}, {'schemaVersion': 2}):
                with self.subTest(manifest=changed), self.assertRaises(ValueError):
                    retain.verify_manifest(json.dumps(changed), image_id)
        restart_fixture(self)
        with self.subTest(phase="mismatched_or_overclaiming_handoff_rejected"):
            for field, value in [('revision', 'd' * 40), ('architecture', 'aarch64'),
                                 ('public_manifest_sha256', 'e' * 64), ('target_update_ref', self.target + '-other'),
                                 ('generation', 8), ('generation', True), ('schema', True),
                                 ('raw_rootfs_id', 'mutable:tag'), ('source_date_epoch', True),
                                 ('source_date_epoch', -1), ('source_date_epoch', 124),
                                 ('installable', True), ('sealed', True),
                                 ('published', True), ('unexpected', 'input')]:
                changed = self.receipt | {field: value}
                with self.subTest(field=field, value=value), self.assertRaises(ValueError):
                    self.validate(changed)
        restart_fixture(self)
        with self.subTest(phase="intact_raw_pair_rejects_split_ambiguous_empty_and_redirected_images"):
            if not (hasattr(os, 'getuid') and os.getuid() == 0):
                self.skipTest('mounted rootfs ownership needs root')
            for failure in (None, 'split', 'ambiguous', 'empty', 'symlink'):
                with self.subTest(failure=failure), tempfile.TemporaryDirectory() as temporary:
                    root = Path(temporary)
                    kernel = root / 'usr/lib/modules/fixture-version'
                    kernel.mkdir(parents=True)
                    for name in ('vmlinuz', 'initramfs.img'):
                        (kernel / name).write_bytes(b'synthetic-not-bootable')
                    if failure == 'split':
                        (kernel / 'vmlinuz').unlink()
                    elif failure == 'ambiguous':
                        (kernel.parent / 'other-version').mkdir()
                    elif failure == 'empty':
                        (kernel / 'initramfs.img').write_bytes(b'')
                    elif failure == 'symlink':
                        (kernel / 'initramfs.img').unlink()
                        (kernel / 'initramfs.img').symlink_to(kernel / 'vmlinuz')
                    if failure:
                        with self.assertRaises((OSError, ValueError)):
                            retain.raw_kernel_pair(root)
                    else:
                        self.assertEqual(retain.raw_kernel_pair(root), 'fixture-version')

    def test_signing_binding(self):
        with self.subTest(phase="signature_binds_exact_image_and_handoff"):
            digest = 'sha256:' + 'd' * 64
            reference = retain.REPOSITORY + '@' + digest
            handoff = retain.receipt_digest(self.receipt)
            payload = {'critical': {'type': 'cosign container image signature',
                                   'image': {'docker-manifest-digest': digest},
                                   'identity': {'docker-reference': retain.REPOSITORY}},
                       'optional': {retain.ANNOTATION: handoff}}
            retain.verify_signature(json.dumps([payload]), reference, handoff)
            for field in ('annotation', 'digest', 'repository'):
                changed = copy.deepcopy(payload)
                if field == 'annotation':
                    changed['optional'][retain.ANNOTATION] = 'f' * 64
                elif field == 'digest':
                    changed['critical']['image']['docker-manifest-digest'] = 'sha256:' + 'e' * 64
                else:
                    changed['critical']['identity']['docker-reference'] += '-other'
                with self.subTest(field=field), self.assertRaises(ValueError):
                    retain.verify_signature(json.dumps([changed]), reference, handoff)
        restart_fixture(self)
        with self.subTest(phase="signing_key_must_match_before_any_registry_command"):
            for matches in (True, False):
                with tempfile.TemporaryDirectory() as temporary:
                    directory = Path(temporary)
                    with patch.object(retain.source, 'command', side_effect=[
                        'derived-public-pem', 'canonical-public',
                        'canonical-public' if matches else 'other-public']) as command:
                        if matches:
                            retain.verify_signing_key(Path('/private/key'), Path('/public/key'), directory, {})
                        else:
                            with self.assertRaisesRegex(ValueError, 'identity differs'):
                                retain.verify_signing_key(Path('/private/key'), Path('/public/key'), directory, {})
                        self.assertEqual(command.call_args_list[0].args[0],
                                         ['cosign', 'public-key', '--key', '/private/key'])
                        self.assertFalse(any('push' in call.args[0] or 'sign' in call.args[0]
                                             for call in command.call_args_list))


if __name__ == '__main__':
    unittest.main()
