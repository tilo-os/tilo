import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import unittest
from unittest.mock import patch

import test_rebind_development_trust as fixtures

SPEC = importlib.util.spec_from_file_location('complete_rebind_gate',
    Path(__file__).resolve().parents[1] / 'dev/complete-initramfs-rebind.py')
gate = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(gate)


class DriverArtifactTests(unittest.TestCase):
    def test_real_archive_rejects_xfs_but_keeps_supported_roots(self):
        import tempfile
        from test_initramfs_trust import cpio, KEY, verifier
        with tempfile.TemporaryDirectory() as directory:
            image = Path(directory) / 'initramfs'
            image.write_bytes(cpio([(verifier.KEY, KEY), ('usr/lib/modules/test/kernel/fs/btrfs/btrfs.ko.zst', b'fixture')]))
            self.assertEqual(gate.verify_artifact(image, KEY)['omitted_kernel_drivers_verified'], ['xfs'])
            image.write_bytes(cpio([(verifier.KEY, KEY), ('usr/lib/modules/test/kernel/fs/ext4/ext4.ko.zst', b'fixture')]))
            self.assertEqual(gate.verify_artifact(image, KEY)['omitted_kernel_drivers_verified'], ['xfs'])
            for driver in ('xfs',):
                for suffix in ('', '.xz', '.gz', '.zst'):
                    image.write_bytes(cpio([(verifier.KEY, KEY), (f'usr/lib/modules/test/kernel/fs/{driver}/{driver}.ko{suffix}', b'fixture')]))
                    with self.subTest(driver=driver, suffix=suffix), self.assertRaisesRegex(ValueError, 'filesystem driver'):
                        gate.verify_artifact(image, KEY)


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


@unittest.skipUnless(hasattr(os, 'getuid') and os.getuid() == 0
                     and shutil.which('cosign') and fixtures.fixtures.trust.CRYPTO.is_file(),
                     'native root Rust-crypto fixture required')
class CompletionTests(unittest.TestCase):
    setUpClass = classmethod(fixtures.RebindTests.setUpClass.__func__)

    def setUp(self):
        fixtures.RebindTests.setUp(self)
        result = fixtures.rebind.replace(self.image, **self.arguments)
        self.selected = dict(disposable=True, marker_sha256=result['pending_marker_sha256'],
            public_bundle=self.new, manifest_sha256=self.arguments['new_manifest'],
            target=self.arguments['new_target'], architecture=self.arguments['architecture'], generation=7)
        self.initramfs = self.image / 'usr/lib/modules/test/initramfs.img'
        self.pending = self.image / gate.PENDING
        self.original_marker = self.pending.read_bytes()
        self.key = (self.new / 'tpm2-pcr-signing.public.pem').read_bytes()
        # Artifact parser is separately tested on real CPIO/squashfs fixtures.
        # These tests exercise the mutation gate, including failed verification.
        self.initramfs.write_bytes(b'newly generated fixture initramfs')
        self.proof = {'initramfs_sha256': hashlib.sha256(self.initramfs.read_bytes()).hexdigest(),
                      'pcr_public_sha256': hashlib.sha256(self.key).hexdigest()}

    def test_marker_cleared_only_by_verified_completion(self):
        with self.subTest(phase="verified_bytes_clear_marker_but_do_not_claim_boot"):
            with patch.object(gate, 'verify_artifact', return_value=self.proof) as verify:
                result = gate.complete(self.image, **self.selected)
                verify.assert_called_once_with(self.initramfs, self.key)
            self.assertFalse(self.pending.exists())
            self.assertEqual(result['artifact'], self.proof)
            self.assertFalse(result['boot_proven'])
            self.assertFalse(result['installable'])
            self.assertEqual(json.loads((self.image / gate.VERIFIED).read_text()), result)
            gate.rebind.retention.source.mounted_identity(self.image, self.selected['target'],
                self.selected['manifest_sha256'], self.selected['architecture'])
        restart_fixture(self)
        with self.subTest(phase="invalid_independent_inputs_never_call_verifier_or_clear_marker"):
            for field, value in [('marker_sha256', '0' * 64), ('manifest_sha256', '0' * 64),
                                 ('generation', 8), ('target', 'registry.tilo.so/tilo/tilo:latest'),
                                 ('disposable', False), ('architecture', 'foreign')]:
                with self.subTest(field=field), patch.object(gate, 'verify_artifact') as verify:
                    with self.assertRaises(ValueError):
                        gate.complete(self.image, **(self.selected | {field: value}))
                    verify.assert_not_called()
                self.assertEqual(self.pending.read_bytes(), self.original_marker)
                self.assertFalse((self.image / gate.VERIFIED).exists())
        restart_fixture(self)
        with self.subTest(phase="bad_artifact_or_mismatched_proof_keeps_signing_blocked"):
            for proof in (None, self.proof | {'initramfs_sha256': '0' * 64},
                          self.proof | {'pcr_public_sha256': '0' * 64}):
                with self.subTest(proof=proof), patch.object(gate, 'verify_artifact',
                        side_effect=ValueError('stale key') if proof is None else None,
                        return_value=proof):
                    with self.assertRaises(ValueError):
                        gate.complete(self.image, **self.selected)
                self.assertEqual(self.pending.read_bytes(), self.original_marker)
        restart_fixture(self)
        with self.subTest(phase="changed_kernel_pending_or_artifact_cannot_clear_gate"):
            for what in ('kernel', 'marker', 'initramfs', 'generation'):
                kernel = self.image / 'usr/lib/modules/test/vmlinuz'
                generation = self.image / 'usr/lib/tilo/generation'
                originals = {path: path.read_bytes() for path in (kernel, self.pending, self.initramfs, generation)}
                def changed(*_):
                    path = {'kernel': kernel, 'marker': self.pending, 'initramfs': self.initramfs,
                            'generation': generation}[what]
                    path.write_bytes(b'changed during verification')
                    return self.proof
                with self.subTest(what=what), patch.object(gate, 'verify_artifact', side_effect=changed):
                    with self.assertRaises(ValueError):
                        gate.complete(self.image, **self.selected)
                    self.assertTrue(self.pending.exists())
                for path, data in originals.items():
                    path.write_bytes(data)
        restart_fixture(self)
        with self.subTest(phase="final_identity_failure_restores_pending_gate"):
            with patch.object(gate, 'verify_artifact', return_value=self.proof), \
                    patch.object(gate.rebind.retention.source, 'mounted_identity', side_effect=ValueError('changed')):
                with self.assertRaises(ValueError):
                    gate.complete(self.image, **self.selected)
            self.assertEqual(self.pending.read_bytes(), self.original_marker)


if __name__ == '__main__':
    unittest.main()
