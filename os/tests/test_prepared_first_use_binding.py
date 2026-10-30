#!/usr/bin/env python3
"""Native CLI regressions for the prepared profile binding."""
import hashlib
import json
import os
from pathlib import Path
import tempfile
import subprocess
import unittest

FILE = 'prepared-setup.sealed'
RECEIPT_FILE = 'receipt.json'


class NativeProfileBinding(unittest.TestCase):
    def preflight(self, root, succeeds=True):
        binary = os.environ.get('TILO_PREPARED_EXTRACT_BINARY', str(
            Path(__file__).resolve().parents[2] / 'target/debug/tilo-installer-prepared-setup-extract'))
        result = subprocess.run([binary, 'preflight-profile', '--source-dir', str(root)],
                                capture_output=True, check=False)
        if succeeds:
            self.assertEqual(result.returncode, 0, result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, b'')

    def test_prepared_profile_binding_is_preserved_and_mutations_fail_closed(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            ciphertext = b'TILOMIG1' + b'encrypted-transport-fixture' * 3
            sealed = root / FILE
            sealed.write_bytes(ciphertext)
            sealed.chmod(0o600)
            expected = dict(schema=1, sealed_bytes=len(ciphertext),
                            ciphertext_sha256=hashlib.sha256(ciphertext).hexdigest())
            profile = dict(schema=1, purpose='server', device_label='Travel computer',
                           prepared_setup=expected)
            (root / 'profile.json').write_text(json.dumps(profile))
            self.preflight(root)
            self.assertEqual(json.loads((root / RECEIPT_FILE).read_text()), expected)
            for mutation in (dict(purpose='other-os'), dict(password='never-log'),
                             dict(device_label='bad\nlabel'), dict(prepared_setup=None)):
                changed = dict(profile, **mutation)
                (root / 'profile.json').write_text(json.dumps(changed))
                with self.subTest(fields=list(mutation)):
                    self.preflight(root, succeeds=False)
                self.assertEqual(json.loads((root / RECEIPT_FILE).read_text()), expected)
            (root / 'profile.json').write_text(json.dumps(profile))
            sealed.write_bytes(b'x' * len(ciphertext))
            self.preflight(root, succeeds=False)
            self.assertEqual(json.loads((root / RECEIPT_FILE).read_text()), expected)

    def test_legacy_profile_without_preparation_still_passes(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / 'profile.json').write_text(json.dumps(dict(schema=1, hostname='legacy')))
            self.preflight(root)
            self.assertFalse((root / RECEIPT_FILE).exists())

if __name__ == '__main__':
    unittest.main()
