"""Pure admission fixtures plus read-only regular-file handling; no loop operations."""
import copy
from contextlib import nullcontext
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch
from test_development_boot import synthetic_uki
from restart_fixture import restart_fixture

DEV = Path(__file__).resolve().parents[1] / 'dev'
sys.path.insert(0, str(DEV))
spec = importlib.util.spec_from_file_location('installed_disk', DEV / 'verify-installed-development-disk.py')
disk = importlib.util.module_from_spec(spec)
spec.loader.exec_module(disk)


class Layout(unittest.TestCase):
    def setUp(self):
        self.table = {'partitiontable': {'label': 'gpt', 'device': '/dev/loop7', 'sectorsize': 512,
            'partitions': [{'node': '/dev/loop7p1', 'name': 'EFI-SYSTEM',
                'type': 'c12a7328-f81f-11d2-ba4b-00a0c93ec93b', 'start': 2048, 'size': 2097152},
                {'node': '/dev/loop7p2', 'name': 'root', 'type': '7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab',
                 'start': 2099200, 'size': 10000000}]}}

    def test_partition_layout(self):
        with self.subTest(phase="exact_arm_layout"):
            self.assertEqual(disk.partitions(self.table, '/dev/loop7'), ['/dev/loop7p1', '/dev/loop7p2'])
        restart_fixture(self)
        with self.subTest(phase="exact_install_media_layout"):
            table = copy.deepcopy(self.table)
            table['partitiontable']['partitions'][0].update(name='esp', size=4194304)
            table['partitiontable']['partitions'][1].update(name='Tilo root', start=4196352)
            self.assertEqual(disk.media_partitions(table, '/dev/loop7'),
                             ['/dev/loop7p1', '/dev/loop7p2'])
        restart_fixture(self)
        with self.subTest(phase="reject_other_devices_types_overlap_and_extra_partition"):
            for field, value in [('node', '/dev/nvme0n1p2'), ('type', 'wrong'), ('start', 2048)]:
                document = copy.deepcopy(self.table)
                document['partitiontable']['partitions'][1][field] = value
                with self.assertRaises(ValueError): disk.partitions(document, '/dev/loop7')
            self.table['partitiontable']['partitions'].append({})
            with self.assertRaises(ValueError): disk.partitions(self.table, '/dev/loop7')
        restart_fixture(self)
        with self.subTest(phase="duplicate_loader_directive"):
            with self.assertRaises(ValueError): disk.config_lines(b'efi /one\nefi /two\n')

    def test_boot_material_binding(self):
        with self.subTest(phase="cheap_preflight_mounts_read_only_and_parses_arm64_uki"):
            digest = 'a' * 128
            uki = synthetic_uki('aarch64', f'quiet composefs={digest}'.encode())
            with tempfile.TemporaryDirectory() as temporary:
                image = Path(temporary) / 'disk.raw'
                with image.open('wb') as stream:
                    stream.truncate(3 * 1024**3)

                def command(argv, **_kwargs):
                    if argv[:4] == ['/usr/sbin/losetup', '--find', '--show', '--read-only']:
                        return b'/dev/loop7\n'
                    if argv[:2] == ['/usr/sbin/blockdev', '--getro']:
                        return b'1\n'
                    if argv[:2] == ['/usr/sbin/sfdisk', '--json']:
                        table = copy.deepcopy(self.table)
                        table['partitiontable']['partitions'][0].update(name='esp', size=4194304)
                        table['partitiontable']['partitions'][1].update(name='Tilo root', start=4196352)
                        return json.dumps(table).encode()
                    if argv[0] == '/usr/bin/mount':
                        destination = Path(argv[-1])
                        if argv[2] == 'vfat':
                            (destination / 'loader/entries').mkdir(parents=True)
                            (destination / 'loader/entries/tilo.conf').write_text(
                                f'title Tilo\nuki /EFI/Linux/bootc/bootc_composefs-{digest}.efi\n')
                            (destination / 'loader/loader.conf').write_text('timeout 1\n')
                            target = destination / f'EFI/Linux/bootc/bootc_composefs-{digest}.efi'
                            target.parent.mkdir(parents=True)
                            target.write_bytes(uki)
                        else:
                            (destination / 'composefs').mkdir()
                            (destination / 'state/deploy').mkdir(parents=True)
                        return b''
                    if argv[:2] == ['/usr/bin/findmnt', '--json']:
                        mountpoint = argv[argv.index('--mountpoint') + 1]
                        filesystem = 'vfat' if mountpoint.endswith('/esp') else 'btrfs'
                        return json.dumps({'filesystems': [{'target': mountpoint,
                            'fstype': filesystem, 'options': 'ro,nodev,nosuid,noexec'}]}).encode()
                    if argv[0] in ('/usr/bin/umount', '/usr/sbin/losetup'):
                        return b''
                    raise AssertionError(argv)

                with patch.object(disk.os, 'getuid', return_value=0), \
                        patch.object(disk.os, 'geteuid', return_value=0), \
                        patch.object(disk, 'require_private_namespace'), \
                        patch.object(disk, '_directory', side_effect=lambda path:
                            os.open(path, os.O_RDONLY | os.O_DIRECTORY)), \
                        patch.object(disk, 'readable_partition_nodes', return_value=nullcontext(
                            [Path('/private/loop7p1'), Path('/private/loop7p2')])), \
                        patch.object(disk, 'command', side_effect=command), \
                        patch.object(disk.hardware, 'trusted_path', side_effect=lambda root, relative, **_kw:
                            root / relative):
                    receipt = disk.preflight_disk(image.resolve())
                self.assertEqual(receipt['purpose'], 'tilo-disk-boot-preflight')
                self.assertEqual(receipt['uki']['architecture'], 'aarch64')
                self.assertEqual(receipt['composefs_digest'], digest)
                self.assertTrue(all(row['read_only_mount_verified'] for row in receipt['partitions']))
        restart_fixture(self)
        with self.subTest(phase="material_rejection_is_not_swallowed"):
            with patch.object(disk.hardware, 'trusted_path', side_effect=lambda root, relative, **kw: root / relative), \
                    patch.object(disk.material, 'audit_installed_material', side_effect=disk.material.AuditError('rejected')):
                with self.assertRaises(disk.material.AuditError):
                    disk.audit_material(Path('/root-view'), Path('/esp'), 'target.efi', 'a' * 128, 'aarch64')
        restart_fixture(self)
        with self.subTest(phase="origin_binds_manifest_and_actual_boot_sections"):
            sections = {'.linux': b'kernel', '.initrd': b'initrd'}
            digest = hashlib.sha256(b'kernelinitrd').hexdigest()
            raw = f'[origin]\ncontainer-image-reference=ostree-unverified-image:docker://registry/tilo:dev\n[image]\nmanifest_digest=sha256:abc\n[boot]\nboot_type=uki\ndigest={digest}\n'.encode()
            disk.origin_identity(raw, 'registry/tilo:dev', 'sha256:abc', sections)
            for target, manifest, parts in [('registry/other:dev', 'sha256:abc', sections),
                    ('registry/tilo:dev', 'sha256:other', sections),
                    ('registry/tilo:dev', 'sha256:abc', sections | {'.initrd': b'changed'})]:
                with self.assertRaises(ValueError): disk.origin_identity(raw, target, manifest, parts)
        restart_fixture(self)
        with self.subTest(phase="copy_receipt_binds_original_receipt_and_destination_identity"):
            path=Path('/work/disk-output/disk.raw')
            source={'device':1,'inode':2,'size':512}
            destination={'device':3,'inode':4,'size':512}
            creation={'path':str(path),'file_identity':source,'size_bytes':512,'disk_created':True}
            raw=(json.dumps(creation,sort_keys=True)+'\n').encode()
            copied={'schema':1,'purpose':'tilo-supervised-raw-media-copy',
              'source_creation_sha256':hashlib.sha256(raw).hexdigest(),
              'source_path':str(path),'source_file_identity':source,
              'raw_identity':{'size':512,'sha256':'a'*64},'path':str(path),
              'file_identity':destination}
            access=disk.copied_disk_receipt(path,raw,creation,copied)
            self.assertEqual(access['file_identity'],destination)
            for key,value in [('source_creation_sha256','b'*64),('source_file_identity',destination),
                              ('path','/other/disk.raw')]:
                hostile=copy.deepcopy(copied);hostile[key]=value
                with self.subTest(key=key),self.assertRaisesRegex(ValueError,'does not bind'):
                    disk.copied_disk_receipt(path,raw,creation,hostile)


@unittest.skipUnless(sys.platform == 'linux' and os.geteuid() == 0, 'root Linux file ownership checks')
class Reopen(unittest.TestCase):
    def test_source_and_uki_selection(self):
        with self.subTest(phase="materialized_target_requires_all_exact_source_files"):
            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                source_root, esp = root / 'source', root / 'esp'
                original = source_root / 'boot/tilo-target'
                copied = esp / 'tilo-target'
                for base in (original, copied):
                    (base / 'predictions').mkdir(parents=True)
                    (base / 'tilo-target.efi').write_bytes(b'verified-target')
                    (base / 'predictions/generation-7.json').write_bytes(b'prediction')
                    (base / 'predictions/generation-7.json.uki.sig').write_bytes(b'signature')
                proof = {'target_uki_sha256': hashlib.sha256(b'verified-target').hexdigest()}
                result = disk.verify_materialized_target(source_root, esp, proof, 7)
                self.assertEqual(len(result['files']), 3)
                self.assertFalse(result['runtime_access_verified'])
                (copied / 'predictions/generation-7.json').write_bytes(b'wrong')
                with self.assertRaisesRegex(ValueError, 'differs'):
                    disk.verify_materialized_target(source_root, esp, proof, 7)
                (copied / 'predictions/generation-7.json').write_bytes(b'prediction')
                (copied / 'extra.efi').write_bytes(b'extra')
                with self.assertRaisesRegex(ValueError, 'inventory'):
                    disk.verify_materialized_target(source_root, esp, proof, 7)
        with self.subTest(phase="live_media_and_target_keep_distinct_authenticated_identities"):
            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                target = root / disk.prediction.TARGET
                live_path = 'boot/EFI/Linux/tilo-live.efi'
                live = root / live_path
                target.parent.mkdir(parents=True)
                live.parent.mkdir(parents=True)
                base = 'quiet tilo.generation=7 composefs=' + 'a'*128
                target.write_bytes(synthetic_uki('aarch64', base.encode()))
                live.write_bytes(synthetic_uki('aarch64', (base + ' tilo.install').encode()))
                target_sha = hashlib.sha256(target.read_bytes()).hexdigest()
                live_sha = hashlib.sha256(live.read_bytes()).hexdigest()
                evidence = {'target_uki_sha256': target_sha,
                            'signed_ukis': [{'path': live_path, 'sha256': live_sha}]}
                args = (root, evidence)
                normal = disk.source_boot_selection(*args, 'installed-target', None, 'aarch64', 7, 'a'*128)
                medium = disk.source_boot_selection(*args, 'installer-media', live_path, 'aarch64', 7, 'a'*128)
                self.assertEqual(normal['sha256'], target_sha)
                self.assertEqual(medium['sha256'], live_sha)
                self.assertTrue(normal['pcr_prediction_applies'])
                self.assertFalse(medium['pcr_prediction_applies'])
                self.assertNotEqual(live_sha, target_sha)
                for profile, path in [('installed-target', live_path), ('installer-media', None),
                                      ('installer-media', '../arbitrary.efi'),
                                      ('installer-media', 'boot/EFI/Linux/other.efi')]:
                    with self.assertRaises(ValueError):
                        disk.source_boot_selection(*args, profile, path, 'aarch64', 7, 'a'*128)
                live.write_bytes(target.read_bytes())
                with self.assertRaisesRegex(ValueError, 'changed'):
                    disk.source_boot_selection(*args, 'installer-media', live_path, 'aarch64', 7, 'a'*128)
                evidence['signed_ukis'][0]['sha256'] = target_sha
                with self.assertRaisesRegex(ValueError, 'activation'):
                    disk.source_boot_selection(*args, 'installer-media', live_path, 'aarch64', 7, 'a'*128)
        with self.subTest(phase="installer_flag_must_be_one_bare_token"):
            base = 'tilo.generation=7 composefs=' + 'a'*128
            for suffix in ('', ' tilo.install=0', ' tilo.install=1', ' tilo.install tilo.install'):
                with self.assertRaises(ValueError):
                    disk.profile_cmdline((base + suffix).encode(), 'installer-media', 7, 'a'*128)
            with self.assertRaises(ValueError):
                disk.profile_cmdline((base + ' tilo.install').encode(), 'installed-target', 7, 'a'*128)
        with self.subTest(phase="signed_companion_is_selected_and_unsigned_copy_rejected"):
            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                source_root, esp, public = root / 'source', root / 'esp', root / 'public'
                original = source_root / 'usr/lib/systemd/boot/efi/systemd-bootaa64.efi'
                original.parent.mkdir(parents=True)
                original.write_bytes(b'unsigned-original')
                signed = Path(str(original) + '.signed')
                signed.write_bytes(b'verified-signed-companion')
                installed = [esp / p for p in ('EFI/systemd/systemd-bootaa64.efi', 'EFI/BOOT/BOOTAA64.EFI')]
                for path in installed:
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_bytes(signed.read_bytes())
                evidence = {'signed_sha256': hashlib.sha256(signed.read_bytes()).hexdigest()}
                with patch.object(disk.prediction.boot.trust, 'validate', return_value={'identities': {'efi-signing.crt': 'a'*64}}), \
                        patch.object(disk.prediction.boot.loader, 'verify', return_value=evidence) as verify:
                    self.assertEqual(disk.verify_loaders(source_root, esp, public, 'b'*64, 'aarch64'), evidence)
                    verify.assert_called_once_with(original, signed, public / 'efi-signing.crt', 'a'*64, 'aarch64')
                    installed[1].write_bytes(original.read_bytes())
                    with self.assertRaisesRegex(ValueError, 'signed companion'):
                        disk.verify_loaders(source_root, esp, public, 'b'*64, 'aarch64')
        with self.subTest(phase="selected_uki_rejects_alternates_and_overrides"):
            with tempfile.TemporaryDirectory() as temporary:
                esp = Path(temporary)
                (esp / 'loader/entries').mkdir(parents=True)
                (esp / 'EFI/Linux/bootc').mkdir(parents=True)
                digest = 'a' * 128
                target = f'EFI/Linux/bootc/bootc_composefs-{digest}.efi'
                (esp / target).write_bytes(b'fixture')
                entry = esp / 'loader/entries/bootc_tilo-1.conf'
                entry.write_text(f'title Tilo\nversion 1\nsort-key tilo\nefi /{target}\n')
                (esp / 'loader/loader.conf').write_text('timeout 5\n')
                self.assertEqual(disk.selected_uki(esp, digest), target)
                # Exact systemd259 bootctl template can precede bootc's own entry.
                (esp / 'loader/loader.conf').write_text('#timeout 3\n#console-mode keep\ndefault tilo-*\n')
                self.assertEqual(disk.selected_uki(esp, digest), target)
                (esp / 'loader/loader.conf').write_text('default @saved\n')
                with self.assertRaises(ValueError): disk.selected_uki(esp, digest)
                (esp / 'loader/loader.conf').write_text('default tilo-*\n')
                original = entry.read_text()
                entry.write_text(original + 'options init=/bin/sh\n')
                with self.assertRaises(ValueError): disk.selected_uki(esp, digest)
                entry.write_text(original)
                (esp / 'EFI/Linux/alternate.efi').write_bytes(b'alternate')
                with self.assertRaises(ValueError): disk.selected_uki(esp, digest)

    def test_file_and_command_bounds(self):
        with self.subTest(phase="command_enforces_output_cap_and_deadline"):
            self.assertEqual(disk.command([sys.executable, '-c', 'print("ok")']), b'ok\n')
            with self.assertRaisesRegex(ValueError, 'output limit'):
                disk.command([sys.executable, '-c', 'print("x" * 100000)'], output_limit=16)
            with self.assertRaisesRegex(ValueError, 'deadline'):
                disk.command([sys.executable, '-c', 'import time; time.sleep(10)'], timeout=0.05)
        with self.subTest(phase="exact_file_read_only_and_replacements_rejected"):
            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                path = root / 'disk.raw'
                path.write_bytes(b'0' * 512)
                path.chmod(0o600)
                receipt = {'path': str(path), 'file_identity': disk.file_identity(path.stat()),
                           'size_bytes': 512, 'disk_created': True}
                # Only ancestry is mocked: /tmp is intentionally not a creation root.
                with patch.object(disk, '_directory', side_effect=lambda p: os.open(p, os.O_RDONLY | os.O_DIRECTORY)):
                    with disk.open_created_disk(path, receipt) as fd:
                        self.assertEqual(os.read(fd, 1), b'0')
                        with self.assertRaises(OSError): os.write(fd, b'x')
                    path.rename(root / 'old')
                    path.write_bytes(b'0' * 512); path.chmod(0o600)
                    with self.assertRaises(ValueError):
                        with disk.open_created_disk(path, receipt): pass
                    path.unlink(); path.symlink_to(root / 'old')
                    with self.assertRaises(OSError):
                        with disk.open_created_disk(path, receipt): pass
        with self.subTest(phase="hardlink_and_changed_file_rejected"):
            with tempfile.TemporaryDirectory() as temporary:
                path = Path(temporary) / 'disk.raw'
                path.write_bytes(b'0' * 512); path.chmod(0o600)
                receipt = {'path': str(path), 'file_identity': disk.file_identity(path.stat()),
                           'size_bytes': 512, 'disk_created': True}
                with patch.object(disk, '_directory', side_effect=lambda p: os.open(p, os.O_RDONLY | os.O_DIRECTORY)):
                    os.link(path, path.with_name('alias'))
                    with self.assertRaises(ValueError):
                        with disk.open_created_disk(path, receipt): pass
                    path.with_name('alias').unlink()
                    with self.assertRaisesRegex(ValueError, 'modified'):
                        with disk.open_created_disk(path, receipt):
                            before = path.stat()
                            path.write_bytes(b'1' * 512)
                            os.utime(path, ns=(before.st_atime_ns, before.st_mtime_ns + 1000000000))


class InstallerBootcIsolation(unittest.TestCase):
    def receipt(self, helper_sha):
        artifact = {
            'schema': 1, 'purpose': disk.installer_bootc_helper.PURPOSE,
            'architecture': 'aarch64',
            'bootc_version': disk.installer_bootc_helper.BOOTC_VERSION,
            'source_tag': disk.installer_bootc_helper.SOURCE_TAG,
            'source_commit': disk.installer_bootc_helper.SOURCE_COMMIT,
            'source_cargo_lock_sha256': disk.installer_bootc_helper.SOURCE_CARGO_LOCK_SHA256,
            'patch_sha256': disk.installer_bootc_helper.PATCH_SHA256,
            'binary_sha256': helper_sha, 'binary_build_id': 'ab' * 20,
            'rustc_version': 'rustc 1.91.0', 'cargo_version': 'cargo 1.91.0',
            'rust_toolchain': disk.installer_bootc_helper.RUST_TOOLCHAIN,
            'build_image': disk.installer_bootc_helper.BUILD_IMAGE,
            'build_target': disk.installer_bootc_helper.TARGETS['aarch64'],
            'build_command': disk.installer_bootc_helper.BUILD_COMMAND,
        }
        identity = {'architecture': 'aarch64', 'sha256': helper_sha,
                    'build_id': artifact['binary_build_id']}
        runtime = {'schema': 1, 'purpose': 'tilo-installer-bootc-runtime',
                   'binary': identity,
                   'mount': {'mount_point': '/run/tilo-installer-bootc',
                             'read_only': True}}
        original = {'architecture': 'aarch64', 'sha256': '1' * 64,
                    'build_id': 'cd' * 20}
        return {'installer_bootc_helper': {'artifact': artifact,
                'source_binary': original, 'runtime': runtime}}, original

    def test_installed_binary_matches_source_and_helper_is_absent(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source, installed, mutable, esp = [root / name for name in
                                                ('source', 'installed', 'mutable', 'esp')]
            for path in (source / 'usr/bin', installed / 'usr/bin', mutable, esp):
                path.mkdir(parents=True)
            (source / 'usr/bin/bootc').write_bytes(b'original')
            (installed / 'usr/bin/bootc').write_bytes(b'original')
            (mutable / 'state').write_bytes(b'state')
            helper_sha = hashlib.sha256(b'helper').hexdigest()
            receipt, original = self.receipt(helper_sha)
            with patch.object(disk.installer_bootc_helper, 'binary_identity',
                              return_value=original):
                proof = disk.verify_installer_helper_isolation(
                    receipt, source, installed, [mutable], esp, 'aarch64')
            self.assertTrue(proof['helper_absent'])
            (esp / 'leaked-helper').write_bytes(b'helper')
            with patch.object(disk.installer_bootc_helper, 'binary_identity',
                              return_value=original), self.assertRaisesRegex(
                                  ValueError, 'leaked'):
                disk.verify_installer_helper_isolation(
                    receipt, source, installed, [mutable], esp, 'aarch64')


if __name__ == '__main__': unittest.main()
