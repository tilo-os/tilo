"""The release preparer cannot bless caller-selected native executable bytes."""
import importlib.util
from pathlib import Path
import tempfile
import unittest

SOURCE = Path(__file__).resolve().parents[1] / 'release/prepare_whistle_aot_package.py'
SPEC = importlib.util.spec_from_file_location('prepare_whistle_aot_package', SOURCE)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class AotPackageCustody(unittest.TestCase):
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
