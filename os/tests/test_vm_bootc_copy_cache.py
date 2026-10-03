"""The VM retry cache must reuse only one exact booted/cached image pair."""

import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


HELPER = Path(__file__).resolve().parents[1] / 'usr/libexec/tilo-vm-bootc-copy-cache.py'
SPEC = importlib.util.spec_from_file_location('tilo_vm_bootc_copy_cache', HELPER)
cache = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(cache)
real_enabled = cache.enabled


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


class VmBootcCopyCacheTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.cmdline = self.root / 'cmdline'
        self.cmdline.write_text('root=tilo-auto composefs=' + 'a' * 128 + '\n')
        self.receipt = self.root / 'receipt.json'
        self.status = {
            'status': {'booted': {
                'image': {
                    'image': {'image': 'localhost/tilo-installer:latest',
                              'transport': 'containers-storage'},
                    'imageDigest': 'sha256:' + 'b' * 64,
                },
                'ostree': {'checksum': 'c' * 64},
            }}
        }
        self.inspect = [{'Id': 'sha256:' + 'd' * 64}]
        self.manifest = {'schemaVersion': 2,
                         'layers': [{'digest': 'sha256:' + 'e' * 64,
                                     'size': 123}]}
        self.patchers = [
            patch.object(cache, 'CMDLINE', self.cmdline),
            patch.object(cache, 'RECEIPT', self.receipt),
            patch.object(cache, 'enabled', return_value=True),
            patch.object(cache, 'owned_private_file',
                         side_effect=lambda path: Path(path) == self.receipt
                         and self.receipt.exists()),
            patch.object(cache, 'run', side_effect=self.fake_run),
        ]
        for patcher in self.patchers:
            patcher.start()
            self.addCleanup(patcher.stop)

    def fake_run(self, *args):
        if args[:3] == ('bootc', 'status', '--json'):
            return json.dumps(self.status).encode()
        if args[:3] == ('podman', 'image', 'inspect'):
            return json.dumps(self.inspect).encode()
        if args[:3] == ('skopeo', 'inspect', '--raw'):
            return json.dumps(self.manifest).encode()
        raise AssertionError(args)

    def save_current(self):
        self.receipt.write_text(json.dumps(cache.current_receipt()))

    def test_cache_reuse_and_opt_in(self):
        with self.subTest(phase="exact_exported_image_and_booted_identity_can_be_reused"):
            self.save_current()
            self.assertTrue(cache.can_reuse())
        restart_fixture(self)
        with self.subTest(phase="opt_in_absent_copies"):
            self.save_current()
            with patch.object(cache, 'enabled', return_value=False):
                self.assertFalse(cache.can_reuse())
        restart_fixture(self)
        with self.subTest(phase="runtime_opt_in_requires_root_owned_marker_and_vm"):
            with patch.object(cache.os, 'geteuid', return_value=0, create=True), \
                    patch.object(cache, 'owned_private_file', return_value=True), \
                    patch.object(cache, 'run', return_value=b''):
                self.assertTrue(real_enabled())
                with patch.object(cache, 'owned_private_file', return_value=False):
                    self.assertFalse(real_enabled())
                with patch.object(cache.os, 'geteuid', return_value=1000):
                    self.assertFalse(real_enabled())
                with patch.object(cache, 'run', side_effect=OSError('physical host')):
                    self.assertFalse(real_enabled())

    def test_identity_changes_force_copy(self):
        with self.subTest(phase="missing_receipt_copies"):
            self.assertFalse(cache.can_reuse())
        restart_fixture(self)
        with self.subTest(phase="changed_booted_image_digest_copies"):
            self.save_current()
            self.status['status']['booted']['image']['imageDigest'] = 'sha256:' + 'f' * 64
            self.assertFalse(cache.can_reuse())
        restart_fixture(self)
        with self.subTest(phase="changed_signed_composefs_identity_copies"):
            self.save_current()
            self.cmdline.write_text('root=tilo-auto composefs=' + 'f' * 128 + '\n')
            self.assertFalse(cache.can_reuse())
        restart_fixture(self)
        with self.subTest(phase="changed_podman_image_id_copies"):
            self.save_current()
            self.inspect[0]['Id'] = 'sha256:' + 'f' * 64
            self.assertFalse(cache.can_reuse())
        restart_fixture(self)
        with self.subTest(phase="changed_layer_manifest_copies"):
            self.save_current()
            self.manifest['layers'][0]['digest'] = 'sha256:' + 'f' * 64
            self.assertFalse(cache.can_reuse())
        restart_fixture(self)
        with self.subTest(phase="malformed_receipt_copies"):
            self.receipt.write_text('{broken')
            self.assertFalse(cache.can_reuse())


if __name__ == '__main__':
    unittest.main()
