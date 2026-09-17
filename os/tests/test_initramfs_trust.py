import gzip
import importlib.util
from pathlib import Path
import stat
import lzma
from unittest.mock import patch
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location('initramfs', Path(__file__).resolve().parents[1] / 'dev/initramfs-trust.py')
verifier = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(verifier)
KEY = b'-----BEGIN PUBLIC KEY-----\nfixture public bytes\n-----END PUBLIC KEY-----\n'


def cpio(entries):
    result = bytearray()
    for index, (name, content) in enumerate([*entries, ('TRAILER!!!', b'')]):
        encoded = name.encode() + b'\0'
        fields = [index, stat.S_IFREG | 0o644, 0, 0, 1, 0, len(content), 0, 0, 0, 0, len(encoded), 0]
        header = b'070701' + ''.join(f'{field:08x}' for field in fields).encode() + encoded
        result.extend(header + bytes((-len(header)) % 4))
        result.extend(content + bytes((-len(content)) % 4))
    return bytes(result) + bytes((-len(result)) % 512)


class InitramfsTests(unittest.TestCase):
    def verify(self, data):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'initramfs'
            path.write_bytes(data)
            return verifier.verify(path, KEY, scratch_parent=Path(temporary))

    def test_plain_and_early_plus_gzip_exact_bytes(self):
        main = cpio([(verifier.KEY, KEY)])
        for data, count in [(main, 1), (gzip.compress(main), 1), (lzma.compress(main), 1),
                            (cpio([('early_cpio', b'1')]) + gzip.compress(main), 2)]:
            result = self.verify(data)
            self.assertEqual(result['cpio_archives'], count)
            self.assertEqual(result['matching_occurrences'], 1)

    def test_missing_stale_duplicate_corrupt_and_truncated_rejected(self):
        good = cpio([(verifier.KEY, KEY)])
        compressed = bytearray(gzip.compress(good))
        compressed[-5] ^= 1
        cases = [cpio([('other', KEY)]), cpio([(verifier.KEY, b'old key')]),
                 cpio([(verifier.KEY, KEY), (verifier.KEY, KEY)]),
                 cpio([(verifier.KEY, KEY)]) + gzip.compress(good),
                 bytes(compressed), gzip.compress(good)[:-4], good[:100],
                 gzip.compress(good) + b'garbage', b'unsupported format']
        for index, data in enumerate(cases):
            with self.subTest(index=index), self.assertRaises(verifier.VerificationError):
                self.verify(data)

    def test_expansion_budget_and_child_deadline_reject(self):
        main = gzip.compress(cpio([(verifier.KEY, KEY)]))
        with patch.object(verifier, 'MAX_EXPANDED', 32):
            # Default argument is fixed at definition; an oversized entry still
            # fails against the patched explicit per-entry bound.
            with self.assertRaises(verifier.VerificationError):
                self.verify(main)
        with patch.object(verifier, 'TIMEOUT', 0):
            with self.assertRaises(verifier.VerificationError):
                self.verify(main)

    def test_unsupported_erofs_and_path_traversal_fail(self):
        for name in ('erofs-root.img', '../' + verifier.KEY, '/' + verifier.KEY, 'usr//share/tilo/key'):
            with self.subTest(name=name), self.assertRaises(verifier.VerificationError):
                self.verify(cpio([(name, KEY)]))

if __name__ == '__main__':
    unittest.main()
