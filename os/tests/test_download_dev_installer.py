"""Real bounded ZIP64/zstd extraction without an archive cache."""
import hashlib
import importlib.util
import io
from pathlib import Path
import random
import shutil
import subprocess
import tempfile
import unittest
import zipfile

SPEC = importlib.util.spec_from_file_location(
    'download', Path(__file__).resolve().parents[1] / 'dev/download-dev-installer.py')
download = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(download)


class NonSeekable(io.BytesIO):
    def seek(self, *args):
        raise io.UnsupportedOperation('stream')


def fixture(*, raw_checksum=None, name=None):
    raw = random.Random(7).randbytes(256 * 1024)
    compressed = subprocess.run(['zstd', '-q', '-c'], input=raw,
                                check=True, capture_output=True).stdout
    source = b'build_source=' + b'a' * 40 + b'\n'
    authority = {'source_sha256': hashlib.sha256(source).hexdigest(),
                 download.NAME + '.sha256': raw_checksum or hashlib.sha256(raw).hexdigest(),
                 download.NAME + '.zst.sha256': hashlib.sha256(compressed).hexdigest()}
    wire = NonSeekable()
    with zipfile.ZipFile(wire, 'w', compression=zipfile.ZIP_DEFLATED) as archive:
        archive.writestr('source.txt', source)
        for suffix in ('.sha256', '.zst.sha256'):
            archive.writestr(download.NAME + suffix,
                             authority[download.NAME + suffix] + '  '
                             + (download.NAME + suffix).removesuffix('.sha256') + '\n')
        with archive.open(name or download.NAME + '.zst', 'w', force_zip64=True) as output:
            output.write(compressed)
    return raw, authority, wire.getvalue()


@unittest.skipUnless(shutil.which('zstd'), 'native zstd CLI required')
class DownloadTests(unittest.TestCase):
    def extract(self, wire, output, authority, reserve=0):
        response = io.BytesIO(wire)
        return download.extract(response, response.read(65536), output, authority, reserve)

    def test_zip64_stream_has_both_full_hashes_and_only_disk_output(self):
        raw, authority, wire = fixture()
        with tempfile.TemporaryDirectory() as temp:
            output = Path(temp) / 'disk.qcow2'
            receipt = self.extract(wire, output, authority)
            self.assertEqual(output.read_bytes(), raw)
            self.assertEqual(receipt['sha256'], hashlib.sha256(raw).hexdigest())
            self.assertFalse(receipt['archive_cached'])
            self.assertEqual(list(Path(temp).iterdir()), [output])
            with self.assertRaisesRegex(ValueError, 'new file'):
                self.extract(wire, output, authority)

    def test_crc_truncation_and_full_checksum_fail_closed(self):
        _, authority, wire = fixture()
        corrupt = bytearray(wire)
        corrupt[corrupt.rfind(b'PK\x07\x08') + 4] ^= 1
        _, wrong_authority, wrong_hash = fixture(raw_checksum='f' * 64)
        for body, expected in ((bytes(corrupt), authority), (wire[:1000], authority),
                               (wrong_hash, wrong_authority)):
            with self.subTest(bytes=len(body)), tempfile.TemporaryDirectory() as temp:
                with self.assertRaisesRegex(ValueError, 'extraction failed|checksum mismatch'):
                    self.extract(body, Path(temp) / 'disk.qcow2', expected)

    def test_authority_filename_and_space_refusals(self):
        _, authority, wire = fixture()
        _, _, wrong_name = fixture(name='../unexpected.zst')
        with tempfile.TemporaryDirectory() as temp:
            output = Path(temp) / 'disk.qcow2'
            with self.assertRaisesRegex(ValueError, 'free space'):
                self.extract(wire, output, authority, reserve=10**20)
            self.assertFalse(output.exists())
            wrong_authority = dict(authority, source_sha256='f' * 64)
            with self.assertRaisesRegex(ValueError, 'live CI authority'):
                self.extract(wire, output, wrong_authority)
            self.assertFalse(output.exists())
            with self.assertRaisesRegex(ValueError, 'extraction failed'):
                self.extract(wrong_name, output, authority)


if __name__ == '__main__':
    unittest.main()
