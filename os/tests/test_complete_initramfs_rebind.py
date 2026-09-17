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


@unittest.skipUnless(hasattr(os, 'getuid') and os.getuid() == 0 and shutil.which('openssl'),
                     'native root public trust fixture required')
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

    def test_verified_bytes_clear_marker_but_do_not_claim_boot(self):
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

    def test_invalid_independent_inputs_never_call_verifier_or_clear_marker(self):
        for field, value in [('marker_sha256', '0' * 64), ('manifest_sha256', '0' * 64),
                             ('generation', 8), ('target', 'code.podesta.ai/qos/tilo:latest'),
                             ('disposable', False), ('architecture', 'foreign')]:
            with self.subTest(field=field), patch.object(gate, 'verify_artifact') as verify:
                with self.assertRaises(ValueError):
                    gate.complete(self.image, **(self.selected | {field: value}))
                verify.assert_not_called()
            self.assertEqual(self.pending.read_bytes(), self.original_marker)
            self.assertFalse((self.image / gate.VERIFIED).exists())

    def test_bad_artifact_or_mismatched_proof_keeps_signing_blocked(self):
        for proof in (None, self.proof | {'initramfs_sha256': '0' * 64},
                      self.proof | {'pcr_public_sha256': '0' * 64}):
            with self.subTest(proof=proof), patch.object(gate, 'verify_artifact',
                    side_effect=ValueError('stale key') if proof is None else None,
                    return_value=proof):
                with self.assertRaises(ValueError):
                    gate.complete(self.image, **self.selected)
            self.assertEqual(self.pending.read_bytes(), self.original_marker)

    def test_changed_kernel_pending_or_artifact_cannot_clear_gate(self):
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

    def test_final_identity_failure_restores_pending_gate(self):
        with patch.object(gate, 'verify_artifact', return_value=self.proof), \
                patch.object(gate.rebind.retention.source, 'mounted_identity', side_effect=ValueError('changed')):
            with self.assertRaises(ValueError):
                gate.complete(self.image, **self.selected)
        self.assertEqual(self.pending.read_bytes(), self.original_marker)


if __name__ == '__main__':
    unittest.main()
