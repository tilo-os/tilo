import gzip
import io
from pathlib import Path
import stat
import struct
import subprocess
from test_development_boot import synthetic_uki
import sys
import tempfile
import unittest
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'dev'))
import installed_material_audit as audit


def cpio(entries):
    result = bytearray()
    for name, data, mode in [*entries, ('TRAILER!!!', b'', stat.S_IFREG)]:
        name = name.encode() + b'\0'
        fields = (1, mode, 0, 0, 1, 0, len(data), 0, 0, 0, 0, len(name), 0)
        result.extend(b'070701' + b''.join(f'{n:08x}'.encode() for n in fields))
        result.extend(name)
        result.extend(b'\0' * (-(110 + len(name)) % 4))
        result.extend(data)
        result.extend(b'\0' * (-len(data) % 4))
    return bytes(result)


class InstalledAuditTests(unittest.TestCase):
    def test_classifies_public_and_private_material(self):
        with self.subTest(phase="public_keys_and_empty_machine_identity_allowed"):
            image = cpio([('etc/machine-id', b'', stat.S_IFREG),
                          ('usr/public.pem', b'-----BEGIN PUBLIC KEY-----\nabc', stat.S_IFREG)])
            self.assertEqual(audit.Scanner().initramfs(gzip.compress(image))['entries'], 2)
        with self.subTest(phase="private_pem_across_chunks"):
            data = b'x' * (audit.layers.CHUNK - 9) + b'-----BEGIN PRIVATE KEY-----'
            with self.assertRaises(audit.AuditError):
                audit.Scanner().entry('var/innocent', stat.S_IFREG, len(data), io.BytesIO(data))
        with self.subTest(phase="exact_private_bytes"):
            with self.assertRaises(audit.AuditError):
                audit.Scanner([b'private-value']).entry('var/blob', stat.S_IFREG, 13, io.BytesIO(b'private-value'))
        with self.subTest(phase="forbidden_paths_in_actual_initramfs"):
            for path in ('etc/ssh/ssh_host_ed25519_key', 'var/lib/tilo-development/key',
                         'run/tilo-cache-secrets/token', 'root/cosign.key'):
                with self.subTest(path=path), self.assertRaises(audit.AuditError):
                    audit.Scanner().initramfs(cpio([(path, b'', stat.S_IFREG)]))
        with self.subTest(phase="real_squashfs_public_and_hidden_private_material"):
            with tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp) / 'tree'
                root.mkdir()
                (root / 'usr').mkdir()
                (root / 'usr' / 'public').write_bytes(b'-----BEGIN PUBLIC KEY-----')
                (root / 'link').symlink_to('/etc/outside')
                (root / 'literal[*]').write_bytes(b'public wildcard filename')
                image = Path(tmp) / 'payload.img'
                def build():
                    if image.exists():
                        image.unlink()
                    subprocess.run(['/usr/bin/mksquashfs', str(root), str(image), '-noappend',
                                    '-processors', '1', '-quiet'], check=True, stdout=subprocess.DEVNULL,
                                   stderr=subprocess.DEVNULL)
                    return gzip.compress(cpio([('squash-root.img', image.read_bytes(), stat.S_IFREG)]))
                self.assertEqual(audit.Scanner().initramfs(build())['squashfs_images'], 1)
                (root / 'usr' / 'innocent').write_bytes(b'-----BEGIN PRIVATE KEY-----')
                with self.assertRaises(audit.AuditError):
                    audit.Scanner().initramfs(build())
                (root / 'usr' / 'innocent').unlink()
                (root / 'cosign.key').write_bytes(b'opaque binary private key')
                with self.assertRaises(audit.AuditError):
                    audit.Scanner().initramfs(build())
                (root / 'cosign.key').unlink()
                (root / 'ambiguous\nname').write_bytes(b'public')
                with self.assertRaises(audit.AuditError):
                    audit.Scanner().initramfs(build())
        with self.subTest(phase="registry_json_and_nonempty_identity"):
            for name, data in (('var/config.json', b'{"auths":{"registry":{"auth":"abc"}}}'),
                               ('etc/machine-id', b'abcd'), ('var/config.json', b'{malformed')):
                with self.subTest(name=name), self.assertRaises(audit.AuditError):
                    audit.Scanner().entry(name, stat.S_IFREG, len(data), io.BytesIO(data))

    def test_traversal_fails_closed(self):
        with self.subTest(phase="truncated_and_unknown_archive_fail_closed"):
            image = cpio([('etc/foo', b'public', stat.S_IFREG)])
            for data in (image[:-8], b'not-an-initrd', gzip.compress(image)[:-8]):
                with self.subTest(data=len(data)), self.assertRaises((audit.AuditError, audit.archive.VerificationError)):
                    audit.Scanner().initramfs(data)
        with self.subTest(phase="nested_filesystem_not_silently_skipped"):
            with self.assertRaises((audit.AuditError, audit.archive.VerificationError)):
                audit.Scanner().initramfs(cpio([('squash-root.img', b'opaque', stat.S_IFREG)]))
        with self.subTest(phase="tree_does_not_follow_symlink"):
            with tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp) / 'root'
                root.mkdir()
                outside = Path(tmp) / 'private'
                outside.write_bytes(b'-----BEGIN PRIVATE KEY-----')
                (root / 'link').symlink_to(outside)
                audit.Scanner().tree(root, 'var')
                (root / 'actual').write_bytes(outside.read_bytes())
                with self.assertRaises(audit.AuditError):
                    audit.Scanner().tree(root, 'var')
        with self.subTest(phase="actual_selected_uki_and_all_mounted_roots"):
            with tempfile.TemporaryDirectory() as tmp:
                roots = [Path(tmp) / name for name in ('etc', 'var', 'esp')]
                for root in roots:
                    root.mkdir()
                initrd = gzip.compress(cpio([('usr/public', b'public', stat.S_IFREG)]))
                data = bytearray(synthetic_uki('aarch64'))
                struct.pack_into('<I', data, 328 + 40 + 8, len(initrd))
                data[1024:1024 + len(initrd)] = initrd
                uki = roots[2] / 'selected.efi'
                uki.write_bytes(data)
                result = audit.audit_installed_material(*roots, uki)
                self.assertTrue(result['accepted'])
                self.assertEqual(result['initramfs']['entries'], 1)
                for root in roots:
                    bad = root / 'innocent'
                    bad.write_bytes(b'-----BEGIN OPENSSH PRIVATE KEY-----')
                    with self.assertRaises(audit.AuditError):
                        audit.audit_installed_material(*roots, uki)
                    bad.unlink()

if __name__ == '__main__':
    unittest.main()
