"""Checksum admission must execute before extraction, including optimized Python."""
import io
import os
import pathlib
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]


class SourcePinTests(unittest.TestCase):
    def test_bad_archive_refused_before_extraction_in_all_interpreter_modes(self):
        for optimized in (False, True):
            with self.subTest(optimized=optimized), tempfile.TemporaryDirectory() as temp:
                root = pathlib.Path(temp)
                shutil.copyfile(ROOT / 'build_native.py', root / 'build_native.py')
                shutil.copyfile(ROOT / 'source_artifact.py', root / 'source_artifact.py')
                (root / 'vendor').mkdir()
                with tarfile.open(root / 'vendor/ntfs-3g-source.tar.gz', 'w:gz') as archive:
                    marker = tarfile.TarInfo('extraction-must-not-happen')
                    marker.size = 1
                    archive.addfile(marker, io.BytesIO(b'x'))
                output = root / 'output'
                env = os.environ.copy()
                env.pop('PYTHONOPTIMIZE', None)
                if optimized:
                    env['PYTHONOPTIMIZE'] = '1'
                result = subprocess.run(
                    [sys.executable, str(root / 'build_native.py'), '--output', str(output)],
                    env=env, capture_output=True, text=True, check=False,
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('NTFS source checksum mismatch', result.stderr)
                self.assertFalse(output.exists(), 'unverified source reached extraction')


if __name__ == '__main__':
    unittest.main()
