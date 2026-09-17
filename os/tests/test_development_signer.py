#!/usr/bin/env python3
"""Filesystem failure tests; run as root or inside podman unshare."""
import importlib.util
import os
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('signer', Path(__file__).resolve().parents[1] / 'dev/development-signer.py')
signer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(signer)


@unittest.skipUnless(os.getuid() == 0, 'requires root or podman unshare')
class OutputTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.destination = self.root / 'output'

    def test_exclusive_creation_preserves_existing_output(self):
        self.destination.write_bytes(b'existing')
        with self.assertRaises(FileExistsError):
            with signer.exclusive_output(self.destination):
                self.fail('must not open existing output')
        self.assertEqual(self.destination.read_bytes(), b'existing')

    def test_failed_operation_removes_its_own_output(self):
        with self.assertRaises(RuntimeError):
            with signer.exclusive_output(self.destination) as output:
                output.write(b'partial')
                raise RuntimeError('signing failed')
        self.assertFalse(self.destination.exists())

    def test_failed_operation_does_not_remove_replacement(self):
        with self.assertRaises(RuntimeError):
            with signer.exclusive_output(self.destination):
                self.destination.rename(self.root / 'moved')
                self.destination.write_bytes(b'replacement')
                raise RuntimeError('signing failed after rename')
        self.assertEqual(self.destination.read_bytes(), b'replacement')

    def test_redirected_parent_rejected(self):
        link = self.root / 'link'
        link.symlink_to(self.root, target_is_directory=True)
        with self.assertRaises(OSError):
            with signer.exclusive_output(link / 'output'):
                self.fail('must not follow an output parent link')
        self.assertFalse(self.destination.exists())

    def test_nonprivate_parent_rejected(self):
        self.root.chmod(0o755)
        with self.assertRaisesRegex(ValueError, 'owner-only'):
            with signer.exclusive_output(self.destination):
                self.fail('must reject nonprivate output directory')
        self.assertFalse(self.destination.exists())


if __name__ == '__main__':
    unittest.main()
