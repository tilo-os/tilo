"""Synthetic UKI structure/signature tests: these fixtures cannot boot an OS."""
import os
from pathlib import Path
import platform
import shutil
import struct
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'dev'))
import development_boot as boot

SEAL = 'a' * 128


def synthetic_uki(architecture='x86_64', cmdline=None):
    payloads = [(b'.linux', b'synthetic kernel, not executable'),
                (b'.initrd', b'synthetic initramfs, not bootable'),
                (b'.cmdline', cmdline or f'quiet tilo.generation=7 composefs={SEAL}'.encode() + b'\0')]
    data = bytearray(2048)
    data[:2] = b'MZ'
    struct.pack_into('<I', data, 0x3c, 64)
    data[64:68] = b'PE\0\0'
    struct.pack_into('<HHIIIHH', data, 68, boot.loader.MACHINES[architecture], 3, 0, 0, 0, 240, 0x2022)
    optional = 88
    struct.pack_into('<H', data, optional, 0x20b)
    struct.pack_into('<Q', data, optional + 24, 0x100000)
    struct.pack_into('<II', data, optional + 32, 4096, 512)
    struct.pack_into('<II', data, optional + 56, 16384, 512)
    struct.pack_into('<H', data, optional + 68, 10)
    struct.pack_into('<I', data, optional + 108, 16)
    for index, (name, value) in enumerate(payloads):
        header, raw = 328 + index * 40, 512 + index * 512
        data[header:header + 8] = name.ljust(8, b'\0')
        struct.pack_into('<IIII', data, header + 8, len(value), 4096 * (index + 1), 512, raw)
        struct.pack_into('<I', data, header + 36, 0x40000040)
        data[raw:raw + len(value)] = value
    return bytes(data)


class StructureTests(unittest.TestCase):
    def test_uki_structure_parsing(self):
        with self.subTest(phase="both_native_formats_and_exact_command_lines"):
            for architecture in boot.loader.MACHINES:
                sections = boot.uki_sections(synthetic_uki(architecture), architecture)
                boot.validate_cmdline(sections['.cmdline'], 7, SEAL)
        with self.subTest(phase="generation_and_seal_boundaries"):
            for generation in (0, -1, True, '07', '+7', '7\n', 2**64):
                with self.subTest(generation=generation), self.assertRaises(ValueError):
                    boot.expected_inputs('x86_64', generation, SEAL)
            for seal in ('0' * 128, 'A' * 128, 'a' * 127, 'sha512:' + SEAL):
                with self.subTest(seal=seal), self.assertRaises(ValueError):
                    boot.expected_inputs('aarch64', 7, seal)
        with self.subTest(phase="ambiguous_or_incorrect_command_lines_rejected"):
            valid = f'tilo.generation=7 composefs={SEAL}'
            for value in (valid + ' tilo.generation=7', valid + ' composefs=' + SEAL,
                          valid.replace('=7', '=8'), valid.replace('=7', '=07'),
                          valid.replace(SEAL, 'b' * 128), valid + '\0hidden',
                          valid + '\n', '"' + valid + '"', 'quiet'):
                with self.subTest(value=value), self.assertRaises(ValueError):
                    boot.validate_cmdline(value.encode(), 7, SEAL)
        with self.subTest(phase="malformed_geometry_and_payloads_rejected"):
            alterations = [(0, b'XX'), (68, struct.pack('<H', 0xaa64)),
                           (88, struct.pack('<H', 0x10b)), (156, struct.pack('<H', 3)),
                           (368, b'.linux\0\0'), (336, struct.pack('<I', 0)),
                           (348, struct.pack('<I', 511)), (388, struct.pack('<I', 512)),
                           (384, struct.pack('<I', 100000)), (380, struct.pack('<I', 4096))]
            for offset, value in alterations:
                data = bytearray(synthetic_uki())
                data[offset:offset + len(value)] = value
                with self.subTest(offset=offset), self.assertRaises(ValueError):
                    boot.uki_sections(data, 'x86_64')
            for length in (0, 63, 100, 450, 1500):
                with self.subTest(length=length), self.assertRaises(ValueError):
                    boot.uki_sections(synthetic_uki()[:length], 'x86_64')


def restart_fixture(test):
    """Give the next subTest phase of a merged test the fresh fixture it had
    as its own test: tear down, run cleanups, reset instance state, set up."""
    test.tearDown()
    test.doCleanups()
    fresh = type(test)(test._testMethodName).__dict__
    keep = {key: test.__dict__[key] for key in ("_outcome", "_subtest") if key in test.__dict__}
    test.__dict__.clear()
    test.__dict__.update(fresh, **keep)
    test.setUp()


@unittest.skipUnless(hasattr(os, 'getuid') and os.getuid() == 0, 'mounted ownership tests require root')
class MountedInventoryTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.live = self.root / 'boot/EFI/Linux'
        self.live.mkdir(parents=True)
        (self.live / 'one.efi').write_bytes(b'one')

    def test_mounted_inventory_enumeration(self):
        with self.subTest(phase="all_live_and_explicit_target_are_enumerated"):
            (self.live / 'two.EFI').write_bytes(b'two')
            target = self.root / 'boot/tilo-target/tilo-target.efi'
            target.parent.mkdir()
            target.write_bytes(b'target')
            self.assertEqual(len(boot._uki_paths(self.root)), 3)
        restart_fixture(self)
        with self.subTest(phase="redirected_directory_and_target_are_rejected"):
            link = self.live / 'redirect'
            link.symlink_to(self.root, target_is_directory=True)
            with self.assertRaises(ValueError):
                boot._uki_paths(self.root)
            link.unlink()
            target = self.root / 'boot/tilo-target'
            target.symlink_to(self.live, target_is_directory=True)
            with self.assertRaises(ValueError):
                boot._uki_paths(self.root)
        restart_fixture(self)
        with self.subTest(phase="excessive_inventory_is_rejected"):
            for index in range(boot.UKI_COUNT_LIMIT):
                (self.live / f'{index}.efi').write_bytes(b'uki')
            with self.assertRaises(ValueError):
                boot._uki_paths(self.root)


class SignedFixtureTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        missing = [tool for tool in ('cosign',) if not shutil.which(tool)]
        if missing or not Path('/usr/local/bin/tilo-crypto').is_file():
            reason = 'real EFI signature fixture requires cosign and host tilo-crypto'
            if os.environ.get('TILO_REQUIRE_BOOT_SIGNATURE_FIXTURE') == '1':
                raise RuntimeError(reason)
            raise unittest.SkipTest(reason)
        cls.architecture = platform.machine()
        if cls.architecture not in boot.loader.MACHINES:
            raise RuntimeError('real signature fixture requires native x86_64 or aarch64')

    def command(self, *args, env=None):
        subprocess.run(args, check=True, stdout=subprocess.DEVNULL,
                       stderr=subprocess.DEVNULL, env=env)

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='tilo-synthetic-uki-')
        self.addCleanup(self.temporary.cleanup)
        self.work = Path(self.temporary.name)
        self.root = self.work / 'image'
        self.live = self.root / 'boot/EFI/Linux/test.efi'
        self.live.parent.mkdir(parents=True)
        generation = self.root / 'usr/lib/tilo/generation'
        generation.parent.mkdir(parents=True)
        generation.write_text('7\n')
        self.key, self.cert = self.work / 'efi.key', self.work / 'efi.crt'
        self.command('/usr/local/bin/tilo-crypto', 'generate-secure-boot-cert',
                     'Tilo Nonbootable UKI Fixture', str(self.key), str(self.cert))
        public = self.work / 'public.pem'
        self.command('/usr/local/bin/tilo-crypto', 'rsa-public-key', str(self.key), str(public))
        cosign_prefix = self.work / 'cosign'
        environment = os.environ.copy()
        environment['COSIGN_PASSWORD'] = ''
        self.command('cosign', 'generate-key-pair', '--output-key-prefix', str(cosign_prefix),
                     env=environment)
        owner = self.work / 'owner.pub'
        owner.write_text('ab' * 32 + '\n')
        self.bundle = self.work / 'bundle'
        self.manifest = boot.trust.create(
            [Path(str(cosign_prefix) + '.pub'), public, self.cert, owner],
            self.bundle)['manifest_sha256']
        unsigned = self.work / 'unsigned.efi'
        unsigned.write_bytes(synthetic_uki(self.architecture))
        self.command('/usr/local/bin/tilo-crypto', 'sign-efi', str(self.key), str(self.cert),
                     str(unsigned), str(self.live))

    def validate(self):
        return boot.validate_mounted_boot(self.root, self.bundle, self.manifest, self.architecture, 7, SEAL)

    def test_signed_uki_verification(self):
        with self.subTest(phase="real_signed_synthetic_ukis_are_checked_but_never_called_bootable"):
            second = self.live.with_name('second.efi')
            shutil.copyfile(self.live, second)
            target = self.root / 'boot/tilo-target/tilo-target.efi'
            target.parent.mkdir()
            shutil.copyfile(self.live, target)
            result = self.validate()
            self.assertEqual(len(result['ukis']), 3)
            self.assertFalse(result['installable'])
            self.assertFalse(result['proof']['pcr_recomputed'])
            self.assertFalse(result['proof']['composefs_installation'])
            self.assertFalse(result['proof']['vm_boot'])
        restart_fixture(self)
        with self.subTest(phase="unsigned_and_tampered_payloads_are_rejected"):
            signed = self.live.read_bytes()
            for data in (synthetic_uki(self.architecture), signed[:512] + b'X' + signed[513:]):
                self.live.write_bytes(data)
                with self.assertRaises(ValueError):
                    self.validate()
        restart_fixture(self)
        with self.subTest(phase="wrong_certificate_is_rejected"):
            wrong_key, wrong_cert = self.work / 'wrong.key', self.work / 'wrong.crt'
            self.command('/usr/local/bin/tilo-crypto', 'generate-secure-boot-cert',
                         'Wrong Fixture', str(wrong_key), str(wrong_cert))
            unsigned = self.work / 'unsigned.efi'
            self.live.unlink()
            self.command('/usr/local/bin/tilo-crypto', 'sign-efi', str(wrong_key), str(wrong_cert),
                         str(unsigned), str(self.live))
            with self.assertRaises(ValueError):
                self.validate()
        restart_fixture(self)
        with self.subTest(phase="redirects_wrong_generation_and_size_fail_closed"):
            generation = self.root / 'usr/lib/tilo/generation'
            generation.write_text('8\n')
            with self.assertRaises(ValueError):
                self.validate()
            generation.write_text('7\n')
            original = self.work / 'saved.efi'
            self.live.rename(original)
            self.live.symlink_to(original)
            with self.assertRaises(ValueError):
                self.validate()
            self.live.unlink()
            with self.live.open('wb') as stream:
                stream.truncate(boot.UKI_LIMIT + 1)
            with self.assertRaises(ValueError):
                self.validate()


if __name__ == '__main__':
    unittest.main()
