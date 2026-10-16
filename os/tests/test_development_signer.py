#!/usr/bin/env python3
"""Filesystem failure tests; run as root or inside podman unshare."""
import importlib.util
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('signer', Path(__file__).resolve().parents[1] / 'dev/development-signer.py')
signer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(signer)


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


@unittest.skipUnless(os.getuid() == 0, 'requires root or podman unshare')
class OutputTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.destination = self.root / 'output'

    def test_close_failure_removes_only_the_created_output(self):
        original_fdopen = os.fdopen
        class FailedClose:
            def __init__(self, stream):
                self.stream = stream
            def __enter__(self):
                return self.stream
            def __exit__(self, *args):
                self.stream.close()
                raise OSError('delayed buffered write failure')
        for replaced in (False, True):
            with self.subTest(replaced=replaced):
                self.destination.unlink(missing_ok=True)
                def fdopen(*args, **kwargs):
                    return FailedClose(original_fdopen(*args, **kwargs))
                with patch.object(signer.os, 'fdopen', side_effect=fdopen):
                    with self.assertRaisesRegex(OSError, 'delayed'):
                        with signer.exclusive_output(self.destination) as stream:
                            stream.write(b'partial output')
                            if replaced:
                                self.destination.rename(self.root / 'old-output')
                                self.destination.write_bytes(b'replacement')
                if replaced:
                    self.assertEqual(self.destination.read_bytes(), b'replacement')
                else:
                    self.assertFalse(self.destination.exists())

    def test_signer_output_ownership(self):
        with self.subTest(phase="exclusive_creation_preserves_existing_output"):
            self.destination.write_bytes(b'existing')
            with self.assertRaises(FileExistsError):
                with signer.exclusive_output(self.destination):
                    self.fail('must not open existing output')
            self.assertEqual(self.destination.read_bytes(), b'existing')
        restart_fixture(self)
        with self.subTest(phase="failed_operation_removes_its_own_output"):
            with self.assertRaises(RuntimeError):
                with signer.exclusive_output(self.destination) as output:
                    output.write(b'partial')
                    raise RuntimeError('signing failed')
            self.assertFalse(self.destination.exists())
        restart_fixture(self)
        with self.subTest(phase="failed_operation_does_not_remove_replacement"):
            with self.assertRaises(RuntimeError):
                with signer.exclusive_output(self.destination):
                    self.destination.rename(self.root / 'moved')
                    self.destination.write_bytes(b'replacement')
                    raise RuntimeError('signing failed after rename')
            self.assertEqual(self.destination.read_bytes(), b'replacement')
        restart_fixture(self)
        with self.subTest(phase="redirected_parent_rejected"):
            link = self.root / 'link'
            link.symlink_to(self.root, target_is_directory=True)
            with self.assertRaises(OSError):
                with signer.exclusive_output(link / 'output'):
                    self.fail('must not follow an output parent link')
            self.assertFalse(self.destination.exists())
        restart_fixture(self)
        with self.subTest(phase="nonprivate_parent_rejected"):
            self.root.chmod(0o755)
            with self.assertRaisesRegex(ValueError, 'owner-only'):
                with signer.exclusive_output(self.destination):
                    self.fail('must reject nonprivate output directory')
            self.assertFalse(self.destination.exists())


if __name__ == '__main__':
    unittest.main()
