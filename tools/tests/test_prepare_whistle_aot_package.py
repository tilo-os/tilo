"""The release preparer cannot bless caller-selected native executable bytes."""
import importlib.util
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SOURCE = Path(__file__).resolve().parents[1] / 'release/prepare_whistle_aot_package.py'
SPEC = importlib.util.spec_from_file_location('prepare_whistle_aot_package', SOURCE)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class AotPackageCustody(unittest.TestCase):
    def test_image_parents_remain_non_writable_with_group_umask(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            destination = root / 'candidate'
            previous = os.umask(0o002)
            try:
                # Custody rejection is exercised independently below. Replace
                # only fixture input reading to exercise real package creation.
                with patch.object(MODULE, 'verified', return_value=b'fixture'):
                    MODULE.prepare(root / 'runtime', root / 'notices', destination)
            finally:
                os.umask(previous)
            self.assertEqual(destination.stat().st_mode & 0o777, 0o700)
            for path in destination.rglob('*'):
                self.assertEqual(path.stat().st_mode & 0o777,
                                 0o755 if path.is_dir() else 0o644)

    def test_prepare_rejects_forged_native_bytes_before_creating_output(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            runtime = root / 'runtime'
            runtime.mkdir()
            with (runtime / 'libwasmtime.so').open('wb') as stream:
                stream.truncate(MODULE.OBJECTS['libwasmtime.so'][0])
            destination = root / 'must-not-exist'
            with self.assertRaisesRegex(ValueError, 'reviewed source pin'):
                MODULE.prepare(runtime, root / 'notices', destination)
            self.assertFalse(destination.exists())

    def test_prepare_refuses_symlink_and_group_writable_native_input(self):
        for mode in ('symlink', 'group-writable'):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                runtime = root / 'runtime'
                runtime.mkdir()
                path = runtime / 'libwasmtime.so'
                target = root / 'target'
                with target.open('wb') as stream:
                    stream.truncate(MODULE.OBJECTS['libwasmtime.so'][0])
                if mode == 'symlink':
                    path.symlink_to(target)
                else:
                    target.rename(path)
                    path.chmod(0o660)
                destination = root / 'must-not-exist'
                with self.assertRaises((OSError, ValueError)):
                    MODULE.prepare(runtime, root / 'notices', destination)
                self.assertFalse(destination.exists())


if __name__ == '__main__':
    unittest.main()
