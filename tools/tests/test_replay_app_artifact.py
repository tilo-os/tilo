"""Malformed archives and authentic signed-package regression fixtures."""
import hashlib
import importlib.util
import io
from pathlib import Path
import struct
import tarfile
import tempfile
import unittest
import zipfile

from blake3 import blake3
from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat

SPEC = importlib.util.spec_from_file_location('replay', Path(__file__).parents[1] / 'replay_app_artifact.py')
replay = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(replay)


class ReplayTests(unittest.TestCase):
    def test_path_traversal_and_platform_escape_rejected(self):
        for name in ('../bad', '/bad', 'a/../bad', 'C:/bad', 'a\\bad', 'a//bad', './bad'):
            with self.subTest(name=name), self.assertRaises(ValueError):
                replay.safe_path(name)

    def test_zip_checksum_tampering_rejected(self):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d)
            checksum = hashlib.sha256(b'original').hexdigest() + '  index.json\n'
            with zipfile.ZipFile(path / 'input.zip', 'w') as z:
                z.writestr('index.json', b'modified')
                z.writestr('SHA256SUMS', checksum)
            with self.assertRaisesRegex(ValueError, 'SHA256 mismatch'):
                replay.extract(path / 'input.zip', path / 'out', 8 + len(checksum))

    def test_zip_symlinks_rejected_before_extraction(self):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d)
            with zipfile.ZipFile(path / 'input.zip', 'w') as z:
                info = zipfile.ZipInfo('link')
                info.external_attr = 0o120777 << 16
                z.writestr(info, 'elsewhere')
            with self.assertRaisesRegex(ValueError, 'regular ZIP'):
                replay.extract(path / 'input.zip', path / 'out', 9)
            self.assertFalse((path / 'out').exists())

    def package(self, path, mutate=None):
        payload = b'not executable; signed regression payload'
        digest = blake3(payload).digest()
        collection = b'TLOCOL\0\0' + struct.pack('<II', 1, 7) + b'bin/app' + digest
        manifest = b'[package]\nid="dev.test"\nversion="1"\nruntime="tilo-runtime/1"\n[entrypoint]\nbin="bin/app"\n'
        key = Ed25519PrivateKey.from_private_bytes(bytes(range(32)))
        pub = key.public_key().public_bytes(Encoding.Raw, PublicFormat.Raw)
        message = b'TLOPKG\0\0' + struct.pack('<I', 1) + blake3(manifest).digest() + blake3(collection).digest() + b'\1' + pub
        root = message + key.sign(message)
        entries = {'signed-root': root, 'qpkg.toml': ('format=1\npackage_id="qpkg:' + blake3(root).hexdigest() + '"\n').encode(),
                   'manifest.toml': manifest, 'collection.qcl': collection, 'blobs/' + digest.hex(): payload}
        if mutate:
            mutate(entries)
        with tarfile.open(path, 'w') as archive:
            for name, data in entries.items():
                member = tarfile.TarInfo(name)
                member.size = len(data)
                archive.addfile(member, io.BytesIO(data))
        return {'id': 'dev.test', 'version': '1', 'runtime': 'tilo-runtime/1'}

    def test_signed_package_and_tamper_rejection(self):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / 'app.qpkg'
            app = self.package(path)
            self.assertTrue(replay.verify_qpkg(path, app)['package_id'].startswith('qpkg:'))
            self.package(path, lambda e: e.update({'manifest.toml': e['manifest.toml'] + b'# tampered'}))
            with self.assertRaisesRegex(ValueError, 'manifest hash'):
                replay.verify_qpkg(path, app)
            self.package(path, lambda e: e.update({'signed-root': e['signed-root'][:-1] + bytes([e['signed-root'][-1] ^ 1])}))
            with self.assertRaises(InvalidSignature):
                replay.verify_qpkg(path, app)
            self.package(path, lambda e: e.update({next(k for k in e if k.startswith('blobs/')): b'tampered'}))
            with self.assertRaisesRegex(ValueError, 'payload hash'):
                replay.verify_qpkg(path, app)

    def test_runtime_signature_payload_and_unlisted_files(self):
        with tempfile.TemporaryDirectory() as d:
            directory = Path(d)
            bundle = directory / 'tilo-runtime/1'
            bundle.mkdir(parents=True)
            (bundle / 'payload').write_bytes(b'runtime')
            digest = blake3(b'runtime').hexdigest()
            root = blake3(('payload\t' + digest + '\n').encode()).hexdigest()
            key = Ed25519PrivateKey.from_private_bytes(bytes(range(32)))
            pub = key.public_key().public_bytes(Encoding.Raw, PublicFormat.Raw).hex()
            signature = key.sign(blake3(b'tilo-runtime-bundle-v1\0' + b'1' + root.encode()).digest()).hex()
            manifest = f'version="1"\narch="x86_64"\nroot_hash="{root}"\npublisher_hex="{pub}"\nsignature_hex="{signature}"\n[[files]]\npath="payload"\nhash="{digest}"\nsize=7\n'
            (bundle / 'bundle.toml').write_text(manifest)
            (directory / 'runtime-bundle-publisher.hex').write_text(pub)
            self.assertEqual(replay.verify_runtime(directory, 'x86_64')['root_hash'], root)
            (bundle / 'payload').write_bytes(b'changed')
            with self.assertRaisesRegex(ValueError, 'runtime payload mismatch'):
                replay.verify_runtime(directory, 'x86_64')
            (bundle / 'payload').write_bytes(b'runtime')
            (bundle / 'unlisted').write_bytes(b'extra')
            with self.assertRaisesRegex(ValueError, 'coverage mismatch'):
                replay.verify_runtime(directory, 'x86_64')
            (bundle / 'unlisted').unlink()
            (bundle / 'bundle.toml').write_text(manifest.replace(signature, '00' * 64))
            with self.assertRaises(InvalidSignature):
                replay.verify_runtime(directory, 'x86_64')


if __name__ == '__main__':
    unittest.main()
