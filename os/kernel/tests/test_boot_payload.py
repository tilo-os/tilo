"""Exercise the boot probe's production payload extraction with real zstd."""
import io
import json
from pathlib import Path
import struct
import subprocess
import sys
import tarfile
import tempfile
import unittest


class BootPayload(unittest.TestCase):
    def run_payload(self, *, compressed=False, corrupt=False, ambiguous=False,
                    missing=False, symlink=False, invalid_elf=False):
        script = Path(__file__).with_name('boot-smoke.sh').read_text()
        production = script.split("<<'PY'\n", 1)[1].split('\nPY\n', 1)[0]
        unsigned = (b'NOPE' if invalid_elf else b'\x7fELF') + bytes(100)
        signed = unsigned + bytes(32) + bytes(8) + struct.pack('>I', 32)
        signed += b'~Module signature appended~\n'
        payload = signed[:-1] if corrupt else signed
        if compressed:
            payload = subprocess.check_output(['zstd', '--compress', '--stdout'], input=payload)
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / 'out/rootfs').mkdir(parents=True)
            archive_path = root / 'stage.tar.gz'
            prefix = './lib/modules/fixture/'
            with tarfile.open(archive_path, 'w:gz') as archive:
                def add(name, data, link=False):
                    member = tarfile.TarInfo(prefix + name)
                    if link:
                        member.type = tarfile.SYMTYPE
                        member.linkname = '/outside'
                        archive.addfile(member)
                    else:
                        member.size = len(data)
                        archive.addfile(member, io.BytesIO(data))
                add('vmlinuz', b'kernel')
                if not missing:
                    add('kernel/drivers/net/dummy.ko' + ('.zst' if compressed else ''), payload, symlink)
                if ambiguous:
                    add('kernel/drivers/net/dummy.ko.zst', b'other')
            result = subprocess.run([sys.executable, '-c', production, str(archive_path),
                                     'fixture', str(root / 'out')], capture_output=True)
            if corrupt or ambiguous or missing or symlink or invalid_elf:
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse((root / 'out/inputs.json').exists())
            else:
                self.assertEqual(result.returncode, 0, result.stderr.decode())
                self.assertEqual((root / 'out/rootfs/signed.ko').read_bytes(), signed)
                self.assertEqual((root / 'out/rootfs/unsigned.ko').read_bytes(), unsigned)
                self.assertNotEqual((root / 'out/rootfs/tampered.ko').read_bytes(), signed)
                self.assertEqual(json.loads((root / 'out/inputs.json').read_text())['release'], 'fixture')

    def test_plain(self): self.run_payload()
    def test_zstd(self): self.run_payload(compressed=True)
    def test_corrupt_zstd_payload(self): self.run_payload(compressed=True, corrupt=True)
    def test_ambiguous(self): self.run_payload(ambiguous=True)
    def test_missing(self): self.run_payload(missing=True)
    def test_symlink(self): self.run_payload(symlink=True)
    def test_invalid_elf(self): self.run_payload(invalid_elf=True)


if __name__ == '__main__':
    unittest.main()
