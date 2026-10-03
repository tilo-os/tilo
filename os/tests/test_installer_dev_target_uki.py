"""Real filesystem boot representation transition; external ukify is fixture-only."""
import configparser
import hashlib
import importlib.util
import json
import os
import shutil
import stat
import sys
from pathlib import Path
import struct
import subprocess
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / 'usr/libexec/tilo-installer-dev-target-uki.py'
spec = importlib.util.spec_from_file_location('dev_target', SCRIPT)
dev = importlib.util.module_from_spec(spec)
spec.loader.exec_module(dev)
UKI_TOOL = SCRIPT.parents[2] / 'dev/uki-cmdline.py'
KERNEL, INITRD = b'exact CI kernel', b'exact CI initrd'
DTB = struct.pack('>10I', 0xD00DFEED, 40, 40, 40, 40, 17, 16, 0, 0, 0)
DIGEST = 'a' * 128
MANIFEST = 'sha256:' + 'b' * 64
ORIGIN = 'registry.tilo.so/tilo/tilo-vm:main-fixture-lenovo-83ed'


def pe(payloads):
    data = bytearray(4096)
    data[:2] = b'MZ'
    struct.pack_into('<I', data, 60, 64)
    data[64:68] = b'PE\0\0'
    struct.pack_into('<HHIIIHH', data, 68, 0xaa64, len(payloads), 0, 0, 0, 240, 0x2022)
    for index, (name, value) in enumerate(payloads.items()):
        offset, raw = 328 + 40 * index, 1024 + 512 * index
        data[offset:offset + 8] = name.encode().ljust(8, b'\0')
        struct.pack_into('<IIII', data, offset + 8, len(value), 4096 * (index + 1), 512, raw)
        data[raw:raw + len(value)] = value
    return bytes(data)


class DevelopmentTarget(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.image, self.target, self.esp = [self.root / name for name in ('image', 'target', 'esp')]
        self.kdir = self.image / 'usr/lib/modules/7.2.7'
        files = {
            self.kdir / 'vmlinuz': KERNEL,
            self.kdir / 'initramfs.img': INITRD,
            self.kdir / 'dtb/qcom/board.dtb': DTB,
            self.image / 'usr/lib/tilo/image-flavour': b'dev\n',
            self.image / 'usr/lib/os-release': b'ID=tilo\n',
            self.image / 'usr/lib/systemd/boot/efi/linuxaa64.efi.stub': b'stub',
            self.esp / f'EFI/Linux/bootc_composefs-{DIGEST}/vmlinuz': KERNEL,
            self.esp / f'EFI/Linux/bootc_composefs-{DIGEST}/initrd': INITRD,
        }
        for path, data in files.items():
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        deployment = self.target / f'state/deploy/{DIGEST}'
        (deployment / 'etc').mkdir(parents=True)
        self.origin = deployment / f'{DIGEST}.origin'
        self.origin.write_text(f'[origin]\ncontainer-image-reference=ostree-unverified-image:docker://{ORIGIN}\n'
                               f'[image]\nmanifest_digest={MANIFEST}\n[boot]\nboot_type=bls\n'
                               f'digest={hashlib.sha256(KERNEL + INITRD).hexdigest()}\n')
        self.entry = self.esp / 'loader/entries/bootc-tilo-fixture.conf'
        self.entry.parent.mkdir(parents=True)
        self.entry.write_text(f'title Tilo\nversion {DIGEST}\nsort-key bootc-tilo-0\n'
                              f'linux /EFI/Linux/bootc_composefs-{DIGEST}/vmlinuz\n'
                              f'initrd /EFI/Linux/bootc_composefs-{DIGEST}/initrd\n'
                              f'options quiet root=tilo-auto composefs={DIGEST} tilo.install\n')
        self.selected = {'target_digest': MANIFEST, 'update_origin': ORIGIN}
        self.dtb = self.kdir / 'dtb/qcom/x1e80100-lenovo-yoga-slim7x.dtb'
        self.dtb.write_bytes(DTB)
        tools = self.root / 'runtime/usr/lib/tilo'
        tools.mkdir(parents=True)
        builder = (SCRIPT.parents[2] / 'dev/build-custom-installer.sh').read_text()
        start = builder.index('# Runtime validation must survive')
        end = builder.index('install -Dm0755', start)
        env = dict(os.environ, TILO_OS_DIR=str(SCRIPT.parents[2]),
                   INSTALLER_CONTEXT=str(self.root / 'packaging'))
        subprocess.run(['bash', '-eu', '-c', builder[start:end]], env=env, check=True)
        shutil.copytree(self.root / 'packaging/rootfs/usr/lib/tilo', tools, dirs_exist_ok=True)
        # Use the real packaged validator and its real transitive import, with
        # no build-time /usr/local/bin helper available in this runtime fixture.
        self.addCleanup(lambda: sys.modules.pop('hardware_profile', None))
        sys.modules.pop('hardware_profile', None)
        old_path = sys.path[:]
        self.addCleanup(lambda: sys.path.__setitem__(slice(None), old_path))
        for name, value in [('PROFILE_TOOLS', tools), ('UKI_TOOL', UKI_TOOL)]:
            mocked = patch.object(dev, name, value)
            mocked.start()
            self.addCleanup(mocked.stop)
        profile_dir = self.image / 'usr/lib/tilo'
        shutil.copyfile(SCRIPT.parents[2] / 'hardware/profiles/lenovo-83ed.json',
                        profile_dir / 'hardware-profile-definition.json')
        (profile_dir / 'hardware-profile.json').write_text(json.dumps({
            'schema_version': 1, 'id': 'lenovo-83ed', 'architecture': 'aarch64',
            'devicetree': '/' + self.dtb.relative_to(self.image).as_posix(),
            'devicetree_sha256': hashlib.sha256(DTB).hexdigest(),
            'target_update_ref': 'code.tilo.so/tilo/tilo:fixture-lenovo-83ed',
        }))
        # Model immutable root-owned Linux metadata on Windows too. All bytes
        # and validator imports remain real filesystem reads.
        original_lstat = Path.lstat
        def image_lstat(path, *args, **kwargs):
            info = list(original_lstat(path, *args, **kwargs))
            info[0] = stat.S_IFMT(info[0]) | (0o755 if stat.S_ISDIR(info[0]) else 0o644)
            info[4] = 0
            return os.stat_result(info)
        metadata = patch.object(Path, 'lstat', image_lstat)
        metadata.start()
        self.addCleanup(metadata.stop)

    def build(self, argv):
        self.assertEqual(argv[:2], ['ukify', 'build'])
        cmdline = Path(str(argv[argv.index('--cmdline') + 1])[1:]).read_bytes().rstrip(b'\r\n')
        self.assertNotIn(b'tilo.install', cmdline)
        self.assertIn(dev.MOUNT.encode(), cmdline)
        output = Path(argv[argv.index('--output') + 1])
        output.write_bytes(pe({'.linux': KERNEL, '.initrd': INITRD,
                              '.cmdline': cmdline + b'\0', '.dtb': DTB}))
        return ''

    def test_runtime_profile_import_and_pinned_dtb_fail_closed(self):
        self.assertEqual(dev.image_inputs(self.image)[3], self.dtb)
        self.dtb.write_bytes(DTB[:-1] + b'X')
        with self.assertRaisesRegex(ValueError, 'pinned SHA-256'):
            dev.image_inputs(self.image)
        (dev.PROFILE_TOOLS / 'uki_hardware_profile.py').unlink()
        with self.assertRaises(FileNotFoundError):
            dev.image_inputs(self.image)

    def test_conversion_preserves_ci_identity_and_boot_bytes(self):
        with patch.object(dev, 'command', side_effect=self.build):
            self.assertEqual(dev.promote(self.image, self.target, self.esp, self.selected), DIGEST)
        ini = configparser.ConfigParser()
        ini.read(self.origin)
        self.assertEqual(ini.get('boot', 'boot_type'), 'uki')
        self.assertEqual(ini.get('boot', 'digest'), hashlib.sha256(KERNEL + INITRD).hexdigest())
        self.assertEqual(ini.get('image', 'manifest_digest'), MANIFEST)
        self.assertEqual(ini.get('origin', 'container-image-reference'),
                         'ostree-unverified-image:docker://' + ORIGIN)
        self.assertIn(f'efi /EFI/Linux/bootc/bootc_composefs-{DIGEST}.efi', self.entry.read_text())
        self.assertNotIn('linux ', self.entry.read_text())
        self.assertEqual((self.kdir / 'vmlinuz').read_bytes(), KERNEL)
        self.assertFalse((self.image / 'boot').exists())

    def test_sealed_ci_target_projection_retains_origin_and_loader(self):
        image_uki = self.image / 'boot/EFI/Linux/7.2.7.efi'
        image_uki.parent.mkdir(parents=True)
        words = f'root=tilo-auto composefs={DIGEST}'
        original = pe({'.linux': KERNEL, '.initrd': INITRD, '.cmdline': words.encode() + b'\0', '.dtb': DTB})
        image_uki.write_bytes(original)
        target_uki = self.esp / f'EFI/Linux/bootc/bootc_composefs-{DIGEST}.efi'
        target_uki.parent.mkdir(parents=True)
        target_uki.write_bytes(original)
        self.origin.write_text(self.origin.read_text().replace('boot_type=bls', 'boot_type=uki'))
        self.entry.write_text(f'title Tilo\nversion {DIGEST}\nsort-key bootc-tilo-0\nefi /EFI/Linux/bootc/bootc_composefs-{DIGEST}.efi\n')
        old_origin, old_entry = self.origin.read_bytes(), self.entry.read_bytes()
        with patch.object(dev, 'command', side_effect=self.build):
            self.assertEqual(dev.promote(self.image, self.target, self.esp, self.selected), DIGEST)
        self.assertEqual(self.origin.read_bytes(), old_origin)
        self.assertEqual(self.entry.read_bytes(), old_entry)
        self.assertEqual(image_uki.read_bytes(), original)
        self.assertIn(dev.MOUNT.encode(), dev.sections(target_uki)['.cmdline'])

    def test_wrong_payload_or_ci_manifest_fails_before_mutation(self):
        original_entry, original_origin = self.entry.read_bytes(), self.origin.read_bytes()
        for variant in ('payload', 'manifest', 'composefs', 'ambiguous-kernel', 'unsafe-bls'):
            with self.subTest(variant=variant):
                if variant == 'payload':
                    path = self.esp / f'EFI/Linux/bootc_composefs-{DIGEST}/vmlinuz'
                    path.write_bytes(b'other image kernel')
                elif variant == 'manifest':
                    self.selected['target_digest'] = 'sha256:' + 'c' * 64
                elif variant == 'composefs':
                    self.entry.write_text(original_entry.decode().replace('composefs=' + DIGEST, 'composefs=' + 'c' * 128))
                elif variant == 'ambiguous-kernel':
                    (self.image / 'usr/lib/modules/other').mkdir()
                else:
                    self.entry.write_text(original_entry.decode().replace('linux /EFI/', 'linux /../EFI/'))
                with patch.object(dev, 'command') as build, self.assertRaises(ValueError):
                    dev.promote(self.image, self.target, self.esp, self.selected)
                build.assert_not_called()
                self.assertEqual(self.origin.read_bytes(), original_origin)
                if variant == 'payload': path.write_bytes(KERNEL)
                if variant == 'manifest': self.selected['target_digest'] = MANIFEST
                if variant == 'ambiguous-kernel': (self.image / 'usr/lib/modules/other').rmdir()
                self.entry.write_bytes(original_entry)

    def test_unrelated_mount_survives_and_conflicting_boot_mount_fails_before_writes(self):
        original = self.entry.read_text()
        unrelated = 'systemd.mount-extra=LABEL=DATA:/srv:ext4:ro'
        self.entry.write_text(original.rstrip('\n') + ' ' + unrelated + '\n')
        with patch.object(dev, 'command', side_effect=self.build):
            dev.promote(self.image, self.target, self.esp, self.selected)
        uki = self.esp / f'EFI/Linux/bootc/bootc_composefs-{DIGEST}.efi'
        self.assertIn(unrelated.encode(), dev.sections(uki)['.cmdline'])
        # A fresh split-kernel fixture avoids conflating the mount guard with
        # the completed previous transition's already-existing UKI guard.
        uki.unlink()
        self.origin.write_text(self.origin.read_text().replace('boot_type = uki', 'boot_type = bls'))
        self.entry.write_text(original.rstrip('\n') + ' systemd.mount-extra=LABEL=OTHER:/boot:ext4:rw\n')
        origin, entry = self.origin.read_bytes(), self.entry.read_bytes()
        with patch.object(dev, 'command') as build, self.assertRaisesRegex(ValueError, 'conflicting /boot'):
            dev.promote(self.image, self.target, self.esp, self.selected)
        build.assert_not_called()
        self.assertEqual(self.origin.read_bytes(), origin)
        self.assertEqual(self.entry.read_bytes(), entry)
        self.assertFalse(uki.exists())

    def test_empty_efi_is_not_admitted_as_a_sealed_image(self):
        placeholder = self.image / 'boot/EFI/Linux/7.2.7.efi'
        placeholder.parent.mkdir(parents=True)
        placeholder.touch()
        with self.assertRaisesRegex(ValueError, 'missing target boot input'):
            dev.image_inputs(self.image)

    def test_ukify_wrong_dtb_is_rejected_before_boot_metadata_changes(self):
        entry, origin = self.entry.read_bytes(), self.origin.read_bytes()
        def corrupt(argv):
            self.build(argv)
            output = Path(argv[argv.index('--output') + 1])
            output.write_bytes(output.read_bytes().replace(DTB, b'x' * len(DTB)))
            return ''
        with patch.object(dev, 'command', side_effect=corrupt), self.assertRaisesRegex(ValueError, 'hardware DTB'):
            dev.promote(self.image, self.target, self.esp, self.selected)
        self.assertEqual(self.entry.read_bytes(), entry)
        self.assertEqual(self.origin.read_bytes(), origin)
        self.assertFalse((self.esp / f'EFI/Linux/bootc/bootc_composefs-{DIGEST}.efi').exists())

    def test_unsigned_route_requires_real_vm_gate(self):
        with patch.object(dev.os, 'geteuid', return_value=0, create=True), patch.object(dev, 'command') as run:
            with self.assertRaisesRegex(ValueError, 'development installer VM'):
                dev.gate()
            run.assert_not_called()


if __name__ == '__main__':
    unittest.main()
