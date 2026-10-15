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
from types import SimpleNamespace
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / 'usr/libexec/tilo-installer-dev-target-uki.py'
spec = importlib.util.spec_from_file_location('dev_target', SCRIPT)
dev = importlib.util.module_from_spec(spec)
spec.loader.exec_module(dev)
UKI_TOOL = SCRIPT.parents[2] / 'dev/uki-cmdline.py'
BUILDER = SCRIPT.parents[2] / 'dev/dev-target-uki.py'
spec = importlib.util.spec_from_file_location('dev_target_builder', BUILDER)
builder = importlib.util.module_from_spec(spec)
spec.loader.exec_module(builder)
KERNEL, INITRD = b'exact CI kernel', b'exact CI initrd'
DTB = struct.pack('>10I', 0xD00DFEED, 40, 40, 40, 40, 17, 16, 0, 0, 0)
DIGEST = 'a' * 128
MANIFEST = 'sha256:' + 'b' * 64
ORIGIN = 'registry.tilo.so/tilo/tilo-vm:main-fixture-lenovo-83ed'
# What bootc 1.16 `install to-filesystem --composefs-backend` writes ahead of
# the image's kargs in a split-kernel BLS entry (captured from a VM install).
FS_UUID, BOOT_UUID = '298c49ae-60ed-4a00-a035-c863f0b1a686', 'adc2062b-7c7c-4f19-960a-ad87b0823796'
BOOTC_DISK_IDENTITY = f'root=UUID={FS_UUID} rootflags=subvol=/ rw boot=UUID={BOOT_UUID}'


def pe(payloads):
    """A minimal ARM64 PE: each section's raw size is its payload, 512-aligned."""
    raw_sizes = [max(512, -(-len(value) // 512) * 512) for value in payloads.values()]
    data = bytearray(1024 + sum(raw_sizes))
    data[:2] = b'MZ'
    struct.pack_into('<I', data, 60, 64)
    data[64:68] = b'PE\0\0'
    struct.pack_into('<HHIIIHH', data, 68, 0xaa64, len(payloads), 0, 0, 0, 240, 0x2022)
    raw = 1024
    for index, ((name, value), raw_size) in enumerate(zip(payloads.items(), raw_sizes)):
        offset = 328 + 40 * index
        data[offset:offset + 8] = name.encode().ljust(8, b'\0')
        struct.pack_into('<IIII', data, offset + 8, len(value), 4096 * (index + 1), raw_size, raw)
        data[raw:raw + len(value)] = value
        raw += raw_size
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
                              f'options {BOOTC_DISK_IDENTITY} composefs={DIGEST} '
                              f'systemd.mount-extra=UUID={BOOT_UUID}:/boot:auto:ro '
                              f'root=tilo-auto quiet tilo.install\n')
        self.selected = {'target_digest': MANIFEST, 'update_origin': ORIGIN}
        # Split-kernel tests model the constructor alongside the fake build()
        # output. Do not depend on ukify being installed on the CI test runner.
        # Sealed/no-constructor tests override this lookup to return None.
        constructor = patch.object(dev.shutil, 'which', return_value='/fixture/bin/ukify')
        constructor.start()
        self.addCleanup(constructor.stop)
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
        for name, value in [('PROFILE_TOOLS', tools), ('UKI_TOOL', UKI_TOOL),
                            ('TRANSPORT_UKI', self.root / 'no-transport/target-uki.efi')]:
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
        # bootc's install-time disk identity never reaches the installed UKI.
        for word in BOOTC_DISK_IDENTITY.split() + [f'systemd.mount-extra=UUID={BOOT_UUID}:/boot:auto:ro']:
            self.assertNotIn(word.encode(), cmdline.split())
        self.assertTrue(cmdline.startswith(f'composefs={DIGEST} root=tilo-auto quiet '.encode()))
        output = Path(argv[argv.index('--output') + 1])
        output.write_bytes(pe({'.linux': KERNEL, '.initrd': INITRD,
                              '.cmdline': cmdline + b'\0', '.dtb': DTB}))
        return ''

    def test_runtime_profile_import_and_pinned_dtb_fail_closed(self):
        self.assertEqual(dev.image_inputs(self.image)[3], self.dtb)
        self.dtb.write_bytes(DTB[:-1] + b'X')
        with self.assertRaisesRegex(ValueError, 'pinned SHA-256'):
            dev.image_inputs(self.image)
        (dev.PROFILE_TOOLS / 'hardware_profile.py').unlink()
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
        with patch.object(dev, 'command', side_effect=AssertionError('sealed projection must not invoke a constructor')), patch.object(dev.shutil, 'which', return_value=None):
            self.assertEqual(dev.promote(self.image, self.target, self.esp, self.selected), DIGEST)
        self.assertEqual(self.origin.read_bytes(), old_origin)
        self.assertEqual(self.entry.read_bytes(), old_entry)
        self.assertEqual(image_uki.read_bytes(), original)
        self.assertIn(dev.MOUNT.encode(), dev.sections(target_uki)['.cmdline'])
        tool = dev.load_module(UKI_TOOL, 'test_editor')
        hdr, raw, raw_size, _, _ = tool.find_cmdline_room(original)
        unchanged = bytearray(target_uki.read_bytes())
        unchanged[raw:raw + raw_size] = original[raw:raw + raw_size]
        unchanged[hdr + 8:hdr + 12] = original[hdr + 8:hdr + 12]
        self.assertEqual(bytes(unchanged), original)
        # A full command-line section refuses growth without touching the
        # installed UKI, origin or loader; no constructor fallback is allowed.
        full = pe({'.linux': KERNEL, '.initrd': INITRD,
                   '.cmdline': (words + ' x=' + 'z' * (512 - len(words) - 3)).encode(), '.dtb': DTB})
        target_uki.write_bytes(full)
        with self.assertRaisesRegex(SystemExit, 'only 512 are available'):
            dev.promote(self.image, self.target, self.esp, self.selected)
        self.assertEqual(target_uki.read_bytes(), full)
        self.assertEqual(self.origin.read_bytes(), old_origin)
        self.assertEqual(self.entry.read_bytes(), old_entry)

    def test_split_constructor_missing_fails_during_image_preflight(self):
        with patch.object(dev.shutil, 'which', return_value=None):
            with self.assertRaisesRegex(ValueError, 'runtime ukify constructor before installation'):
                dev.image_inputs(self.image)
        self.assertFalse((self.esp / f'EFI/Linux/bootc/bootc_composefs-{DIGEST}.efi').exists())

    def runner_uki(self, **changes):
        """The runner's real constructor; only the external ukify is a fixture."""
        payloads = {}
        def run(argv, **kwargs):
            argv = [str(item) for item in argv]
            if argv[1].endswith('uki-hardware-profile.py'):
                self.assertEqual(argv[2:], [str(self.image), '7.2.7', 'aarch64'])
                return SimpleNamespace(stdout=str(self.dtb) + '\n')
            self.assertEqual(argv[:2], ['ukify', 'build'])
            self.assertEqual(Path(argv[argv.index('--linux') + 1]), self.kdir / 'vmlinuz')
            self.assertEqual(argv[argv.index('--devicetree') + 1], str(self.dtb))
            cmdline = Path(argv[argv.index('--cmdline') + 1][1:]).read_bytes().rstrip(b'\n')
            payloads.update({'.linux': KERNEL, '.initrd': INITRD,
                             '.cmdline': cmdline + b'\0', '.dtb': DTB}, **changes)
            Path(argv[argv.index('--output') + 1]).write_bytes(pe(payloads))
            return SimpleNamespace(stdout='')
        output = self.root / 'transport/target-uki.efi'
        output.parent.mkdir()
        with patch.object(builder, 'run', side_effect=run):
            builder.construct(self.image, output)
        mocked = patch.object(dev, 'TRANSPORT_UKI', output)
        mocked.start()
        self.addCleanup(mocked.stop)
        return output

    def test_runner_built_uki_installs_without_a_live_constructor(self):
        self.assertEqual(builder.CMDLINE_ROOM, dev.CMDLINE_ROOM)
        transported = self.runner_uki()
        original = transported.read_bytes()
        self.assertEqual(dev.sections(transported)['.cmdline'].rstrip(b'\0'),
                         builder.placeholder().encode())
        with patch.object(dev, 'command', side_effect=AssertionError('the live medium has no ukify')), \
                patch.object(dev.shutil, 'which', return_value=None):
            dev.image_inputs(self.image)
            self.assertEqual(dev.promote(self.image, self.target, self.esp, self.selected), DIGEST)
        installed = dev.sections(self.esp / f'EFI/Linux/bootc/bootc_composefs-{DIGEST}.efi')
        self.assertEqual((installed['.linux'], installed['.initrd'], installed['.dtb']), (KERNEL, INITRD, DTB))
        self.assertEqual(installed['.cmdline'],
                         f'composefs={DIGEST} root=tilo-auto quiet {dev.MOUNT}'.encode())
        ini = configparser.ConfigParser()
        ini.read(self.origin)
        self.assertEqual(ini.get('boot', 'boot_type'), 'uki')
        self.assertEqual(ini.get('image', 'manifest_digest'), MANIFEST)
        self.assertIn(f'efi /EFI/Linux/bootc/bootc_composefs-{DIGEST}.efi', self.entry.read_text())
        self.assertEqual(transported.read_bytes(), original)

    def test_runner_built_uki_must_carry_the_selected_payload_and_room(self):
        entry, origin = self.entry.read_bytes(), self.origin.read_bytes()
        for changes, message in (({'.linux': b'other image kernel'}, 'split kernel payload'),
                                 ({'.dtb': b'x' * len(DTB)}, 'pinned hardware DTB'),
                                 ({'.cmdline': b'root=tilo-auto\0'}, 'reserved command-line room')):
            with self.subTest(changes=changes), patch.object(dev.shutil, 'which', return_value=None):
                transported = self.runner_uki(**changes)
                with patch.object(dev, 'command') as build, self.assertRaisesRegex(ValueError, message):
                    dev.promote(self.image, self.target, self.esp, self.selected)
                build.assert_not_called()
                self.assertEqual(self.entry.read_bytes(), entry)
                self.assertEqual(self.origin.read_bytes(), origin)
                self.assertFalse((self.esp / f'EFI/Linux/bootc/bootc_composefs-{DIGEST}.efi').exists())
                shutil.rmtree(transported.parent)

    def test_runner_built_uki_refuses_a_command_line_beyond_its_room(self):
        self.runner_uki(**{'.cmdline': (dev.CMDLINE_ROOM + '=x').encode()})
        entry = self.entry.read_bytes()
        with patch.object(dev.shutil, 'which', return_value=None), \
                self.assertRaisesRegex(ValueError, 'reserved 512'):
            self.entry.write_text(entry.decode().replace(' tilo.install', ' x=' + 'y' * 600 + ' tilo.install'))
            dev.promote(self.image, self.target, self.esp, self.selected)
        self.assertFalse((self.esp / f'EFI/Linux/bootc/bootc_composefs-{DIGEST}.efi').exists())

    def test_runner_built_uki_beside_a_sealed_image_uki_is_ambiguous(self):
        self.runner_uki()
        image_uki = self.image / 'boot/EFI/Linux/7.2.7.efi'
        image_uki.parent.mkdir(parents=True)
        image_uki.write_bytes(pe({'.linux': KERNEL, '.initrd': INITRD, '.cmdline': b'x\0', '.dtb': DTB}))
        with self.assertRaisesRegex(ValueError, 'both a sealed and a transported UKI'):
            dev.image_inputs(self.image)

    def test_wrong_payload_or_ci_manifest_fails_before_mutation(self):
        original_entry, original_origin = self.entry.read_bytes(), self.origin.read_bytes()
        for variant in ('payload', 'manifest', 'composefs', 'ambiguous-kernel', 'unsafe-bls',
                        'other-root', 'second-bootc-root'):
            with self.subTest(variant=variant):
                if variant == 'payload':
                    path = self.esp / f'EFI/Linux/bootc_composefs-{DIGEST}/vmlinuz'
                    path.write_bytes(b'other image kernel')
                elif variant == 'manifest':
                    self.selected['target_digest'] = 'sha256:' + 'c' * 64
                elif variant == 'composefs':
                    self.entry.write_text(original_entry.decode().replace('composefs=' + DIGEST, 'composefs=' + 'c' * 128))
                elif variant == 'other-root':
                    # Only bootc's own UUID root is dropped; any other root stays fatal.
                    self.entry.write_text(original_entry.decode().replace(' quiet', ' root=/dev/vda3 quiet'))
                elif variant == 'second-bootc-root':
                    self.entry.write_text(original_entry.decode().replace(' quiet', f' root=UUID={BOOT_UUID} quiet'))
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
