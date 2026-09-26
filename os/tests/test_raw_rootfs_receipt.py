#!/usr/bin/env python3
"""Public handoff bounds and filesystem destination safety."""
import importlib.util
import json
import os
from pathlib import Path
import platform
import sys
import tempfile
import unittest

DEV = Path(__file__).resolve().parents[1] / 'dev'
sys.path.insert(0, str(DEV))
spec = importlib.util.spec_from_file_location('raw_receipt', DEV / 'raw-rootfs-receipt.py')
receipt = importlib.util.module_from_spec(spec)
spec.loader.exec_module(receipt)


class RawRootfsReceipt(unittest.TestCase):
    def arguments(self):
        return dict(raw_rootfs_id='sha256:' + 'a' * 64, revision='b' * 40,
                    source_date_epoch='1700000000', architecture=platform.machine(), generation='42',
                    target_update_ref='127.0.0.1:5000/tilo-development/os:test', public_manifest_sha256='c' * 64)

    def test_receipt_document_and_output(self):
        with self.subTest(phase="document_binds_inputs_without_attesting_installability"):
            value = receipt.document(**self.arguments())
            self.assertEqual(value['schema'], 1)
            self.assertEqual(value['source_date_epoch'], 1700000000)
            self.assertEqual(value['generation'], 42)
            self.assertEqual(value['raw_rootfs_id'], 'sha256:' + 'a' * 64)
            for name in ('installable', 'sealed', 'published'):
                self.assertFalse(value[name])
        with self.subTest(phase="rejects_mutable_malformed_or_out_of_range_inputs"):
            for name, value in [('raw_rootfs_id', 'localhost/mutable:tag'), ('revision', 'main'),
                                ('source_date_epoch', '253402300800'), ('source_date_epoch', '01'),
                                ('generation', str(2**64)), ('generation', '0'),
                                ('architecture', 'unsupported'), ('public_manifest_sha256', 'secret'),
                                ('target_update_ref', 'registry.tilo.so/tilo/tilo:latest')]:
                with self.subTest(name=name), self.assertRaises(ValueError):
                    receipt.document(**(self.arguments() | {name: value}))
        with self.subTest(phase="exclusive_external_output_rejects_links_and_writable_parent"):
            with tempfile.TemporaryDirectory() as temporary:
                base = Path(temporary)
                checkout = base / 'checkout'
                checkout.mkdir()
                output = base / 'receipt.json'
                value = receipt.document(**self.arguments())
                receipt.write_exclusive(output, value, checkout)
                self.assertEqual(json.loads(output.read_text()), value)
                self.assertEqual(output.stat().st_mode & 0o777, 0o600)
                with self.assertRaises(FileExistsError):
                    receipt.write_exclusive(output, value, checkout)
                output.unlink()
                output.symlink_to(base / 'missing')
                with self.assertRaises(FileExistsError):
                    receipt.write_exclusive(output, value, checkout)
                output.unlink()
                alias = base / 'alias'
                alias.symlink_to(base, target_is_directory=True)
                with self.assertRaises(OSError):
                    receipt.write_exclusive(alias / 'receipt.json', value, checkout)
                for unsafe in (checkout / 'receipt.json', Path('relative.json'), base / '..' / 'receipt.json'):
                    with self.assertRaises(ValueError):
                        receipt.write_exclusive(unsafe, value, checkout)
                base.chmod(0o777)
                try:
                    with self.assertRaises(ValueError):
                        receipt.write_exclusive(output, value, checkout)
                finally:
                    base.chmod(0o700)


if __name__ == '__main__':
    unittest.main()
