"""Host-only binary fixtures; these are not bootable/filesystem-qualified images.

Run: python3 -m unittest discover -s os/tests -p 'internal_bundle_test.py'
(Python 3.11+: the live command-line derivation reads kargs.d with tomllib.)
"""
import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import struct
import tempfile
import unittest
from unittest.mock import patch
import uuid
import zlib

REPO = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('internal_bundle', REPO / 'os/dev/internal-bundle.py')
bundle = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(bundle)
ESP = 'c12a7328-f81f-11d2-ba4b-00a0c93ec93b'
ROOT = '0fc63daf-8483-4772-8e79-3d69d8477de4'
STAGING_ROOT = 'd4c4daf6-f72c-43ee-b8e3-5ba32550311d'
TILO_ROOT = '7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab'
ROOT_UUID = '12345678-1234-4234-8234-123456789abc'
DIGEST = 'b' * 128
# The one fixture both validators accept: apps/imager/src/setup/internal_bundle.rs
# include_str!s this file, so Rust and Python are checked against the same bytes.
SHARED_FIXTURE = REPO / 'os/tests/fixtures/internal-bundle-v2.json'
CONTAINERFILE = REPO / 'os/Containerfile'
INSTALLER_CONTAINERFILE = REPO / 'os/Containerfile.installer'
KARGS = REPO / 'os/usr/lib/bootc/kargs.d'
COMMAND = re.compile(
    r'tilo-uki-cmdline\s+(remove|append)\s+(\S+)(?:\s+(/boot/tilo-target/tilo-target\.efi))?')


# --- the live command line, derived from the files that produce it ------------

def kargs_tokens(generation):
    """kargs.d in bootc's lexical order, including the build-generated generation file."""
    import tomllib
    files = {path.name: tomllib.loads(path.read_text(encoding='utf-8'))['kargs']
             for path in KARGS.glob('*.toml')}
    files['02-generation.toml'] = ['tilo.generation=%d' % generation]
    return [token for name in sorted(files) for token in files[name]]


def apply(tokens, action, argument):
    key = argument.split('=', 1)[0]
    matches = [token for token in tokens if token.split('=', 1)[0] == key]
    if action == 'remove':
        if len(matches) != 1:
            raise AssertionError(f'remove {key} matched {len(matches)} tokens')
        return [token for token in tokens if token.split('=', 1)[0] != key]
    if matches:
        raise AssertionError(f'append {key} would duplicate {matches[0]}')
    return [*tokens, argument]


def live_tokens(digest=DIGEST, generation=7):
    """What os/Containerfile.installer leaves on the live UKI."""
    live = kargs_tokens(generation) + ['composefs=' + digest]
    for action, argument, target in COMMAND.findall(INSTALLER_CONTAINERFILE.read_text(encoding='utf-8')):
        if not target:
            live = apply(live, action, argument)
    return live


def live_cmdline(digest=DIGEST, generation=7):
    return ' '.join(live_tokens(digest, generation))


def shared_fixture():
    return json.loads(SHARED_FIXTURE.read_text(encoding='utf-8'))


def valid_fixture():
    manifest = shared_fixture()
    bundle.validate_manifest(manifest)
    return manifest


class LiveCmdlineTests(unittest.TestCase):
    def test_live_cmdline_derivation(self):
        with self.subTest(phase="kargs_sources_are_the_ones_derived_here"):
            # A new kargs file, or a new generated one, must change this derivation.
            copied = set(re.findall(r'os/usr/lib/bootc/kargs\.d/(\S+\.toml)',
                                    CONTAINERFILE.read_text(encoding='utf-8')))
            self.assertEqual(copied, {path.name for path in KARGS.glob('*.toml')})
            generated = set(re.findall(r'> /usr/lib/bootc/kargs\.d/(\S+\.toml)',
                                       CONTAINERFILE.read_text(encoding='utf-8')))
            self.assertEqual(generated, {'02-generation.toml'})
            installer = INSTALLER_CONTAINERFILE.read_text(encoding='utf-8')
            self.assertEqual(re.findall(r'> /usr/lib/bootc/kargs\.d/(\S+\.toml)', installer),
                             ['50-installer.toml'])
            # That drop-in only repeats what the UKI projection appends.
            self.assertIn('''printf 'kargs = ["tilo.install"]\\n' ''', installer)
            self.assertIn('tilo.install', live_tokens())
        with self.subTest(phase="shared_fixture_carries_the_derived_live_cmdline"):
            fixture = shared_fixture()['boot_binding']['uki_cmdline']
            self.assertEqual(fixture, live_cmdline())
            self.assertIn('root=tilo-auto', fixture.split())
        with self.subTest(phase="validator_accepts_exactly_the_live_projection"):
            for generation in (1, 7, 4294967295):
                with self.subTest(generation=generation):
                    bundle.validate_cmdline(live_cmdline(generation=generation), DIGEST)
            tokens = live_cmdline().split()
            for index in range(len(tokens)):
                if tokens[index].split('=', 1)[0] in ('root', 'composefs', 'tilo.install', 'rw'):
                    with self.subTest(removed=tokens[index]):
                        reduced = ' '.join(tokens[:index] + tokens[index+1:])
                        with self.assertRaises(ValueError):
                            bundle.validate_cmdline(reduced, DIGEST)

    def test_live_cmdline_rejections(self):
        with self.subTest(phase="device_bound_or_placeholder_roots_are_rejected"):
            # The staging UKI is the signed live UKI: it never names a device.
            base = live_cmdline()
            for root in ('root=UUID=' + ROOT_UUID, 'root=PARTUUID=' + ROOT_UUID, 'root=/dev/sda2',
                         'tilo.root=' + '0'*37, 'root=UUID=' + ROOT_UUID + ' tilo.root=' + '0'*37):
                with self.subTest(root=root):
                    with self.assertRaises(ValueError):
                        bundle.validate_cmdline(base.replace('root=tilo-auto', root), DIGEST)
        with self.subTest(phase="live_cmdline_profile_is_closed"):
            base = live_cmdline()
            for change in (' ro', ' rw', ' init=/bin/sh', ' console=tty1', ' tilo.generation=0',
                           ' loglevel=3', ' systemd.show_status=auto', ' rd.udev.log_level=3',
                           ' vt.global_cursor_default=0', ' root=tilo-auto', ' unknown',
                           ' rd.luks.options=tpm2-device=auto,fido2-device=auto',
                           # The unlock slots the live UKI no longer carries.
                           ' tilo.luks=' + '0'*45, ' tilo.unlock=' + '0'*75):
                with self.subTest(change=change):
                    with self.assertRaises(ValueError):
                        bundle.validate_cmdline(base + change, DIGEST)
            for old, new in (('tilo.mode=normal', 'tilo.mode=repair'), ('preempt=full', 'preempt=none'),
                             ('root=tilo-auto', 'root=tilo-auto2'),
                             ('rd.luks.options=tpm2-device=auto,fido2-device=auto',
                              'rd.luks.options=tpm2-device=auto')):
                with self.subTest(old=old):
                    with self.assertRaises(ValueError):
                        bundle.validate_cmdline(base.replace(old, new), DIGEST)


# --- GPT fixtures --------------------------------------------------------------

def gpt_fixture(path, *, overlap=False, root_type=ROOT, root_attributes=0, esp_attributes=0):
    """Real 512-byte-sector GPT encoding, arbitrary partition fixture bytes."""
    data = bytearray(128 * 512)
    data[510:512] = b'\x55\xaa'
    struct.pack_into('<B3sB3sII', data, 446, 0, b'\0'*3, 0xee, b'\0'*3, 1, 127)
    entries = bytearray(128 * 128)
    for i, (kind, first, last) in enumerate([(ESP, 34, 41), (root_type, 40 if overlap else 42, 57)]):
        off = i * 128
        entries[off:off+16] = uuid.UUID(kind).bytes_le
        entries[off+16:off+32] = uuid.UUID(int=i+1).bytes_le
        struct.pack_into('<QQQ', entries, off+32, first, last, esp_attributes if i == 0 else root_attributes)
    data[2*512:34*512] = entries
    data[95*512:127*512] = entries
    disk = uuid.UUID(int=3).bytes_le
    for current, other, table in [(1, 127, 2), (127, 1, 95)]:
        hdr = bytearray(512)
        struct.pack_into('<8sIIIIQQQQ16sQIII', hdr, 0, b'EFI PART', 0x10000, 92, 0, 0,
                         current, other, 34, 94, disk, table, 128, 128, zlib.crc32(entries))
        struct.pack_into('<I', hdr, 16, zlib.crc32(hdr[:92]))
        data[current*512:(current+1)*512] = hdr
    data[34*512:42*512] = b'E' * 4096
    data[42*512:58*512] = b'R' * 8192
    path.write_bytes(data)
    return data


def gpt_disk(path, sectors, partitions):
    """A sparse GPT disk: partitions are (type, first_lba, last_lba, attributes, payload)."""
    entries = bytearray(128 * 128)
    for i, (kind, first, last, attributes, _payload) in enumerate(partitions):
        off = i * 128
        entries[off:off+16] = uuid.UUID(kind).bytes_le
        entries[off+16:off+32] = uuid.UUID(int=i+1).bytes_le
        struct.pack_into('<QQQ', entries, off+32, first, last, attributes)
    with open(path, 'wb') as disk:
        disk.truncate(sectors * 512)
        mbr = bytearray(512)
        mbr[510:512] = b'\x55\xaa'
        struct.pack_into('<B3sB3sII', mbr, 446, 0, b'\0'*3, 0xee, b'\0'*3, 1, sectors - 1)
        disk.write(mbr)
        for current, other, table in [(1, sectors - 1, 2), (sectors - 1, 1, sectors - 33)]:
            hdr = bytearray(512)
            struct.pack_into('<8sIIIIQQQQ16sQIII', hdr, 0, b'EFI PART', 0x10000, 92, 0, 0,
                             current, other, 34, sectors - 34, uuid.UUID(int=3).bytes_le, table,
                             128, 128, zlib.crc32(entries))
            struct.pack_into('<I', hdr, 16, zlib.crc32(hdr[:92]))
            disk.seek(current * 512)
            disk.write(hdr)
            disk.seek(table * 512)
            disk.write(entries)
        for _kind, first, _last, _attributes, payload in partitions:
            for offset, data in payload:
                disk.seek(first * 512 + offset)
                disk.write(data)


def fixture_manifest(image, output):
    """A schema-2 manifest around the structural export_ranges primitive."""
    components = bundle.export_ranges(image, output)
    shared = shared_fixture()
    for component in components:
        # Structural source attributes are not staging flags or a schema expansion.
        component.pop('source_attributes', None)
        reference = next(c for c in shared['components'] if c['role'] == component['role'])
        for field in ('filesystem_type', 'filesystem_uuid', 'partition_type_guid', 'minimum_partition_bytes'):
            component[field] = reference[field]
    manifest = copy.deepcopy(shared)
    manifest['components'] = components
    manifest['source_image_bytes'] = image.stat().st_size
    return manifest


class GptTests(unittest.TestCase):
    def test_gpt_partition_parsing(self):
        with self.subTest(phase="reads_exact_partition_extents"):
            with tempfile.TemporaryDirectory() as tmp:
                path = Path(tmp) / 'fixture.raw'
                gpt_fixture(path)
                self.assertEqual([(p['offset'], p['size']) for p in bundle.read_gpt(path)],
                                 [(17408, 4096), (21504, 8192)])
        with self.subTest(phase="rejects_overlap_crc_damage_and_truncation"):
            with tempfile.TemporaryDirectory() as tmp:
                path = Path(tmp) / 'fixture.raw'
                for kind in ('overlap', 'header', 'table', 'backup', 'truncated'):
                    with self.subTest(kind=kind):
                        data = gpt_fixture(path, overlap=kind == 'overlap')
                        if kind == 'header': data[512+24] ^= 1
                        if kind == 'table': data[1024] ^= 1
                        if kind == 'backup': data[127*512] ^= 1
                        if kind == 'truncated': data = data[:-512]
                        path.write_bytes(data)
                        with self.assertRaises(ValueError): bundle.read_gpt(path)
        with self.subTest(phase="reads_the_live_tilo_root_type_with_grow_attribute"):
            with tempfile.TemporaryDirectory() as tmp:
                path = Path(tmp) / 'fixture.raw'
                gpt_fixture(path, root_type=TILO_ROOT, root_attributes=1 << 59)
                root = bundle.read_gpt(path)[1]
                self.assertEqual((root['role'], root['partition_type_guid'], root['attributes']),
                                 ('staging-root', TILO_ROOT, 1 << 59))


class ExportTests(unittest.TestCase):
    def test_export_bytes_and_inspection(self):
        with self.subTest(phase="exports_exact_bytes_and_validates_fixture"):
            with tempfile.TemporaryDirectory() as tmp:
                image, output = Path(tmp) / 'fixture.raw', Path(tmp) / 'bundle'
                gpt_fixture(image)
                manifest = fixture_manifest(image, output)
                bundle.validate_manifest(manifest)
                self.assertEqual((output / 'staging-esp.img').read_bytes(), b'E'*4096)
                self.assertEqual((output / 'staging-root.img').read_bytes(), b'R'*8192)
                self.assertEqual(manifest['components'][1]['sha256'], hashlib.sha256(b'R'*8192).hexdigest())
                self.assertFalse((output / 'manifest.json').exists())
                (output / 'manifest.json').write_text(json.dumps(manifest))
                self.assertEqual(bundle.inspect_bundle(output / 'manifest.json'), manifest)
                with self.assertRaises(FileExistsError):
                    fixture_manifest(image, output)
                self.assertEqual((output / 'staging-root.img').read_bytes(), b'R'*8192)
        with self.subTest(phase="failed_export_leaves_no_manifest_or_output"):
            with tempfile.TemporaryDirectory() as tmp:
                image, output = Path(tmp) / 'fixture.raw', Path(tmp) / 'bundle'
                gpt_fixture(image)
                gpt_fixture(image, overlap=True)
                with self.assertRaises(ValueError): bundle.export_ranges(image, output)
                self.assertFalse(output.exists())
        with self.subTest(phase="inspection_rejects_changed_bytes_and_symlinks"):
            with tempfile.TemporaryDirectory() as tmp:
                image, output = Path(tmp) / 'fixture.raw', Path(tmp) / 'bundle'
                gpt_fixture(image)
                manifest = fixture_manifest(image, output)
                (output / 'manifest.json').write_text(json.dumps(manifest))
                component = output / 'staging-root.img'
                component.write_bytes(b'X'*8192)
                with self.assertRaises(ValueError): bundle.inspect_bundle(output / 'manifest.json')
                component.unlink()
                component.symlink_to(image)
                with self.assertRaises(ValueError): bundle.inspect_bundle(output / 'manifest.json')


class ManifestTests(unittest.TestCase):
    def test_manifest_validation(self):
        with self.subTest(phase="shared_fixture_is_schema_2"):
            manifest = valid_fixture()
            self.assertEqual((manifest['schema'], manifest['purpose']), (2, 'tilo-internal-installer'))
        with self.subTest(phase="invalid_single_fields"):
            baseline = valid_fixture()
            mutations = [
                ('schema', lambda m: m.update(schema=True)),
                ('schema 1', lambda m: m.update(schema=1)),
                ('unknown', lambda m: m.update(extra=1)),
                ('release bound', lambda m: m.update(release_id='x'*129)),
                ('duplicate roles', lambda m: m['components'][1].update(role='staging-esp')),
                ('unsupported role', lambda m: m['components'][1].update(role='target-root')),
                ('overlap', lambda m: m['components'][1].update(source_offset=17408)),
                ('traversal', lambda m: m['components'][0].update(filename='../outside.img')),
                ('zero', lambda m: m['components'][0].update(byte_size=0)),
                ('too large', lambda m: m['components'][0].update(byte_size=1<<63)),
                ('composefs mismatch', lambda m: m['boot_binding'].update(composefs_digest='c'*128)),
                ('architecture', lambda m: m.update(architecture='aarch64')),
                ('unknown binding', lambda m: m['boot_binding'].update(uki_cmdline='composefs=abc tilo.install')),
                ('duplicate root', lambda m: m['boot_binding'].update(uki_cmdline=m['boot_binding']['uki_cmdline']+' root=UUID='+ROOT_UUID)),
                ('device-bound root', lambda m: m['boot_binding'].update(
                    uki_cmdline=live_cmdline().replace('root=tilo-auto', 'root=UUID='+ROOT_UUID))),
                ('root locator field', lambda m: m['boot_binding'].update(root_locator='UUID='+ROOT_UUID)),
                ('efi escape', lambda m: m['boot_binding'].update(efi_path='/EFI/../evil.efi')),
                ('efi not fallback', lambda m: m['boot_binding'].update(efi_path='/EFI/Linux/fixture.efi')),
                ('efi other arch', lambda m: m['boot_binding'].update(efi_path='/EFI/BOOT/BOOTAA64.EFI')),
                ('efi lower case', lambda m: m['boot_binding'].update(efi_path='/EFI/BOOT/bootx64.efi')),
                ('root is Tilo root type', lambda m: m['components'][1].update(partition_type_guid=TILO_ROOT)),
                ('root is generic Linux type', lambda m: m['components'][1].update(partition_type_guid=ROOT)),
                ('root is discoverable type', lambda m: m['components'][1].update(
                    partition_type_guid='4f68bce3-e8cd-4db1-96e7-fbcaf984b709')),
                ('root below live floor', lambda m: m['components'][1].update(minimum_partition_bytes=(40 << 30) - 512)),
                ('root larger than partition', lambda m: m['components'][1].update(
                    byte_size=(40 << 30) + 512, minimum_partition_bytes=40 << 30)),
                ('esp partition too small', lambda m: m['components'][0].update(minimum_partition_bytes=2048)),
            ]
            for name, change in mutations:
                with self.subTest(name=name):
                    manifest = copy.deepcopy(baseline)
                    change(manifest)
                    with self.assertRaises(ValueError): bundle.validate_manifest(manifest)
        with self.subTest(phase="minimised_root_is_smaller_than_its_partition"):
            manifest = valid_fixture()
            root = manifest['components'][1]
            self.assertLess(root['byte_size'], root['minimum_partition_bytes'])
            self.assertGreaterEqual(root['minimum_partition_bytes'], 40 << 30)
            root['minimum_partition_bytes'] = 64 << 30
            bundle.validate_manifest(manifest)
            root['byte_size'] = root['minimum_partition_bytes'] = 40 << 30
            manifest['source_image_bytes'] = root['source_offset'] + root['byte_size']
            bundle.validate_manifest(manifest)
        with self.subTest(phase="aarch64_binds_its_own_fallback_loader"):
            manifest = valid_fixture()
            manifest['architecture'] = manifest['boot_binding']['uki_architecture'] = 'aarch64'
            with self.assertRaises(ValueError): bundle.validate_manifest(manifest)
            manifest['boot_binding']['efi_path'] = '/EFI/BOOT/BOOTAA64.EFI'
            bundle.validate_manifest(manifest)
        with self.subTest(phase="strict_bounded_json"):
            for raw in (b'{"schema":2,"schema":2}', b'{"x":NaN}', b'['*100+b']'*100, b' '*65537):
                with self.subTest(raw=raw[:30]):
                    with self.assertRaises(ValueError): bundle.parse_manifest(raw)
            manifest = valid_fixture()
            self.assertEqual(bundle.parse_manifest(json.dumps(manifest).encode()), manifest)
            self.assertEqual(bundle.parse_manifest(SHARED_FIXTURE.read_bytes()), manifest)


# --- PE / UKI fixtures ---------------------------------------------------------

def pe_fixture(cmdline=None, machine=0x8664, profile=False):
    """Structurally meaningful PE fixture, not executable kernel/initrd content."""
    cmdline = cmdline or live_cmdline()
    sections = [(b'.linux', b'fixture kernel'), (b'.initrd', b'fixture initrd'),
                (b'.osrel', b'ID=tilo\n'), (b'.cmdline', cmdline.encode()+b'\0')]
    if profile:
        sections.append((b'.profile', b'ID=fixture-profile\n'))
    slot = 512 if all(len(payload) <= 512 for _, payload in sections) else 1024
    data = bytearray(max(4096, 1024 + slot * len(sections)))
    data[:2] = b'MZ'
    struct.pack_into('<I', data, 0x3c, 128)
    data[128:132] = b'PE\0\0'
    struct.pack_into('<HH', data, 132, machine, len(sections))
    struct.pack_into('<HH', data, 148, 240, 0x2022)
    struct.pack_into('<H', data, 152, 0x20b)
    struct.pack_into('<H', data, 152+68, 10)
    for i, (name, payload) in enumerate(sections):
        offset = 1024 + i*slot
        struct.pack_into('<8sIIII', data, 392+i*40, name, len(payload), (i+1)*4096, slot, offset)
        data[offset:offset+len(payload)] = payload
    return data


def with_directories(data):
    """Declare the 16 PE data directories (all empty), as a linker would."""
    data = bytearray(data)
    struct.pack_into('<I', data, 152+108, 16)
    return bytes(data)


def signed(data):
    """Append a canonical WIN_CERTIFICATE table and point the security directory at it."""
    data = bytearray(with_directories(data))
    certificate = struct.pack('<IHH', 8 + 92, 0x0200, 0x0002) + b'\x30' * 92
    certificate += bytes(-len(certificate) % 8)
    struct.pack_into('<II', data, 152+112+4*8, len(data), len(certificate))
    return bytes(data + certificate)


class UkiTests(unittest.TestCase):
    def test_uki_measurement_and_parsing(self):
        with self.subTest(phase="measures_machine_and_exact_cmdline"):
            info = bundle.inspect_uki(bytes(pe_fixture()))
            self.assertEqual(info.get('uki_architecture'), 'x86_64')
            self.assertEqual(info['uki_cmdline'], live_cmdline())
            self.assertEqual(info['uki_sha256'], hashlib.sha256(pe_fixture()).hexdigest())
        with self.subTest(phase="rejects_ambiguous_sections_and_truncation"):
            for problem in ('duplicate', 'overlap', 'truncated', 'wrong machine', 'embedded nul'):
                with self.subTest(problem=problem):
                    data = pe_fixture()
                    if problem == 'duplicate': data[392:400] = b'.cmdline'
                    if problem == 'overlap': struct.pack_into('<I', data, 432+20, 1024)
                    if problem == 'truncated': data = data[:2000]
                    if problem == 'wrong machine': struct.pack_into('<H', data, 132, 0x14c)
                    if problem == 'embedded nul': data = pe_fixture('root=UUID='+ROOT_UUID+'\0 tilo.install')
                    with self.assertRaises(ValueError): bundle.inspect_uki(bytes(data))


# --- filesystem identities -----------------------------------------------------

def fat_boot_sector(serial, fat32=True):
    boot = bytearray(512)
    boot[0:3] = b'\xeb\x58\x90'
    struct.pack_into('<H', boot, 11, 512)
    struct.pack_into('<H', boot, 22, 0 if fat32 else 32)
    extended = 0x42 if fat32 else 0x26
    boot[extended] = 0x29
    struct.pack_into('<I', boot, extended + 1, serial)
    boot[510:512] = b'\x55\xaa'
    return bytes(boot)


def btrfs_block(fsid=ROOT_UUID, total=8 << 20, device_total=None, devices=1, csum_type=0):
    block = bytearray(4096)
    block[0x20:0x30] = uuid.UUID(fsid).bytes
    struct.pack_into('<Q', block, 0x30, 0x10000)
    block[0x40:0x48] = b'_BHRfS_M'
    struct.pack_into('<QQQQ', block, 0x70, total, 1 << 20, 256, devices)
    struct.pack_into('<I', block, 0x90, 4096)
    struct.pack_into('<H', block, 0xc4, csum_type)
    struct.pack_into('<QQ', block, 0xc9, 1, total if device_total is None else device_total)
    struct.pack_into('<I', block, 0, bundle.crc32c(bytes(block[0x20:])))
    return bytes(block)


class FilesystemIdentityTests(unittest.TestCase):
    def test_filesystem_identities_and_size_plan(self):
        with self.subTest(phase="crc32c_matches_the_castagnoli_check_value"):
            self.assertEqual(bundle.crc32c(b'123456789'), 0xe3069283)
        with self.subTest(phase="fat_serial_matches_blkid_format"):
            self.assertEqual(bundle.fat_serial(fat_boot_sector(0x1234abcd)), '1234-ABCD')
            self.assertEqual(bundle.fat_serial(fat_boot_sector(0x00c0ffee, fat32=False)), '00C0-FFEE')
            for boot in (fat_boot_sector(0), fat_boot_sector(1)[:510] + b'\0\0',
                         fat_boot_sector(1).replace(b'\x29', b'\x28', 1)):
                with self.subTest():
                    with self.assertRaises(ValueError): bundle.fat_serial(boot)
        with self.subTest(phase="btrfs_superblock_identity_and_sizes"):
            facts = bundle.btrfs_superblock(btrfs_block(total=3 << 20))
            self.assertEqual((facts['fsid'], facts['total_bytes'], facts['device_total_bytes']),
                             (ROOT_UUID, 3 << 20, 3 << 20))
            damaged = bytearray(btrfs_block())
            damaged[0x70] ^= 1
            for block in (bytes(damaged), btrfs_block(devices=2), btrfs_block(csum_type=1),
                          btrfs_block(fsid='00000000-0000-0000-0000-000000000000'),
                          btrfs_block().replace(b'_BHRfS_M', b'_BHRfS_X')):
                with self.subTest():
                    with self.assertRaises(ValueError): bundle.btrfs_superblock(block)
        with self.subTest(phase="root_size_plan_is_minimum_plus_slack_aligned_and_bounded"):
            gib, mib = 1 << 30, 1 << 20
            self.assertEqual(bundle.plan_root_size(5 * gib, 40 * gib), 6 * gib)
            self.assertEqual(bundle.plan_root_size(5 * gib + 1, 40 * gib), 6 * gib + mib)
            self.assertEqual(bundle.plan_root_size(39 * gib + 1, 40 * gib), 40 * gib)
            with self.assertRaises(ValueError): bundle.plan_root_size(0, 40 * gib)


# --- the export pipeline, with privileged operations faked --------------------

class FakeSystem:
    """Stands in for loop devices: a 'mount' of an image shows a per-image tree.

    The ESP tree is an ordinary directory copied in and out; the Btrfs 'resize'
    rewrites the image's primary superblock the way the kernel would. Nothing
    here touches a real device.
    """

    def __init__(self, esp_tree, shrink=True):
        self.esp_tree, self.shrink, self.calls, self.devices = esp_tree, shrink, [], {}

    def require_privileges(self, qcow2=False):
        self.calls.append(('privileges', qcow2))

    def attach(self, path):
        device = '/dev/loop%d' % len(self.devices)
        self.devices[device] = Path(path)
        self.calls.append(('attach', Path(path).name))
        return device

    def detach(self, device):
        self.calls.append(('detach', self.devices.pop(device).name))

    def mount(self, device, target, fstype, options):
        self.calls.append(('mount', fstype, options.split(',')[0]))
        self.mounted = (fstype, options)
        if fstype == 'vfat':
            shutil.copytree(self.esp_tree, target, dirs_exist_ok=True)

    def umount(self, target):
        self.calls.append(('umount',))
        if self.mounted[0] == 'vfat':
            if self.mounted[1].startswith('rw'):
                shutil.rmtree(self.esp_tree)
                shutil.copytree(target, self.esp_tree)
            for child in Path(target).iterdir():
                shutil.rmtree(child) if child.is_dir() else child.unlink()

    def btrfs_min_dev_size(self, mountpoint):
        return 2 << 20

    def btrfs_resize(self, mountpoint, size):
        self.calls.append(('resize', size))
        if self.shrink:
            image = next(p for p in self.devices.values() if p.name.endswith('-root.img'))
            facts = bundle.read_btrfs_superblock(image)
            with open(image, 'r+b') as stream:
                stream.seek(bundle.BTRFS_SUPER_OFFSET)
                stream.write(btrfs_block(facts['fsid'], total=size))

    def btrfs_check(self, image):
        self.calls.append(('check', Path(image).name, Path(image).stat().st_size))


def live_disk(path, uki_cmdline=None):
    """A GPT disk shaped like the live installer: ESP + Tilo root (8 MiB, floor patched)."""
    esp_first, esp_last = 2048, 4095
    root_first, root_last = 4096, 4096 + 16383
    gpt_disk(path, 22528, [
        (ESP, esp_first, esp_last, 0, [(0, fat_boot_sector(0x1234abcd))]),
        (TILO_ROOT, root_first, root_last, 1 << 59,
         [(0x10000, btrfs_block(total=8 << 20)), (1 << 20, b'root payload')])])


def esp_tree(root, uki):
    (root / 'EFI/Linux/bootc').mkdir(parents=True)
    (root / 'EFI/BOOT').mkdir(parents=True)
    (root / 'EFI/Linux/bootc/bootc_composefs-fixture.efi').write_bytes(uki)
    (root / 'EFI/BOOT/BOOTX64.EFI').write_bytes(b'systemd-boot fixture')
    (root / 'loader').mkdir()
    (root / 'loader/loader.conf').write_text('timeout 0\n')


class ExportImageTests(unittest.TestCase):
    def export(self, tmp, system, **patches):
        image, output = Path(tmp) / 'live.raw', Path(tmp) / 'bundle'
        live_disk(image)
        values = {'LIVE_ROOT_FLOOR': 8 << 20, 'ROOT_SLACK': 1 << 20}
        values.update(patches)
        with patch.multiple(bundle, **values):
            manifest = bundle.export_image(image, output, 'imager-v1.2.3', 'x86_64', system=system)
            bundle.validate_manifest(manifest)
        return image, output, manifest

    def test_exports_signed_uki_and_minimised_root(self):
        with self.subTest(phase="exports_the_signed_live_uki_unchanged_and_a_minimised_root"):
            with tempfile.TemporaryDirectory() as tmp:
                tree = Path(tmp) / 'esp-tree'
                live_uki = signed(pe_fixture(live_cmdline()))
                esp_tree(tree, live_uki)
                system = FakeSystem(tree)
                image, output, manifest = self.export(tmp, system)
                names = bundle.bundle_names('x86_64')
                self.assertEqual(sorted(p.name for p in output.iterdir()), sorted(names.values()))
                esp, root = manifest['components']
                self.assertEqual((esp['filename'], root['filename']), (names['staging-esp'], names['staging-root']))
                # Root: shrunk to min-dev-size + slack, truncated, checked; partition keeps the live size.
                self.assertEqual(root['byte_size'], 3 << 20)
                self.assertEqual(root['minimum_partition_bytes'], 8 << 20)
                self.assertEqual((output / names['staging-root']).stat().st_size, 3 << 20)
                self.assertEqual(root['filesystem_uuid'], ROOT_UUID)
                self.assertEqual(root['partition_type_guid'], STAGING_ROOT)
                self.assertIn(('resize', 3 << 20), system.calls)
                self.assertEqual(system.calls[-1], ('check', names['staging-root'], 3 << 20))
                self.assertEqual(system.devices, {})
                # ESP: both loader paths hold the signed live UKI, byte for byte.
                self.assertEqual((tree / 'EFI/BOOT/BOOTX64.EFI').read_bytes(), live_uki)
                self.assertEqual((tree / 'EFI/Linux/bootc/bootc_composefs-fixture.efi').read_bytes(), live_uki)
                self.assertEqual((tree / 'loader/loader.conf').read_text(), 'timeout 0\n')
                # The staging helper overwrites these in place; they must exist,
                # be exactly the reserved size, and hold only spaces.
                for placeholder in bundle.STAGING_PLACEHOLDERS:
                    self.assertEqual((tree / placeholder).read_bytes(),
                                     b' ' * bundle.STAGING_PLACEHOLDER_BYTES)
                self.assertEqual(esp['filesystem_uuid'], '1234-ABCD')
                self.assertEqual(esp['byte_size'], esp['minimum_partition_bytes'])
                boot = manifest['boot_binding']
                self.assertEqual(boot['efi_path'], '/EFI/BOOT/BOOTX64.EFI')
                self.assertEqual(boot['uki_sha256'], hashlib.sha256(live_uki).hexdigest())
                self.assertEqual(boot['uki_cmdline'], live_cmdline())
                self.assertNotIn('root_locator', boot)
                self.assertEqual(boot['composefs_digest'], DIGEST)
                # The written manifest is the one returned, and it inspects clean.
                raw = (output / names['manifest']).read_bytes()
                with patch.multiple(bundle, LIVE_ROOT_FLOOR=8 << 20):
                    self.assertEqual(bundle.parse_manifest(raw), manifest)
                    self.assertEqual(bundle.inspect_bundle(output / names['manifest']), manifest)
                self.assertEqual([p.name for p in Path(tmp).iterdir() if p.name.startswith('.tilo-bundle-work-')], [])
                self.assertTrue(image.exists())
        with self.subTest(phase="bundle_names_are_valid_component_filenames"):
            for arch in ('x86_64', 'aarch64'):
                names = bundle.bundle_names(arch)
                for role in ('staging-esp', 'staging-root'):
                    bundle.text(names[role], r'[A-Za-z0-9][A-Za-z0-9._-]*\.img', 128)
                self.assertTrue(names['manifest'].endswith('.json'))

    def test_export_image_refusals(self):
        with self.subTest(phase="refuses_when_btrfs_does_not_reach_the_planned_size"):
            with tempfile.TemporaryDirectory() as tmp:
                tree = Path(tmp) / 'esp-tree'
                esp_tree(tree, bytes(pe_fixture(live_cmdline())))
                with self.assertRaisesRegex(OSError, 'partial output retained.*did not shrink'):
                    self.export(tmp, FakeSystem(tree, shrink=False))
                names = bundle.bundle_names('x86_64')
                self.assertFalse((Path(tmp) / 'bundle' / names['manifest']).exists())
        with self.subTest(phase="refuses_an_esp_without_exactly_one_live_uki_or_fallback"):
            for problem in ('two ukis', 'no fallback', 'device-bound root', 'placeholder root'):
                with self.subTest(problem=problem), tempfile.TemporaryDirectory() as tmp:
                    tree = Path(tmp) / 'esp-tree'
                    esp_tree(tree, bytes(pe_fixture(live_cmdline())))
                    if problem == 'two ukis':
                        (tree / 'EFI/Linux/other.efi').write_bytes(bytes(pe_fixture(live_cmdline())))
                    if problem == 'no fallback':
                        (tree / 'EFI/BOOT/BOOTX64.EFI').unlink()
                    if problem in ('device-bound root', 'placeholder root'):
                        root = 'root=UUID=' + ROOT_UUID if problem == 'device-bound root' else 'tilo.root=' + '0'*37
                        cmdline = live_cmdline().replace('root=tilo-auto', root)
                        (tree / 'EFI/Linux/bootc/bootc_composefs-fixture.efi').write_bytes(bytes(pe_fixture(cmdline)))
                    with self.assertRaisesRegex(OSError, 'partial output retained'):
                        self.export(tmp, FakeSystem(tree))
                    self.assertFalse((Path(tmp) / 'bundle' / bundle.bundle_names('x86_64')['manifest']).exists())
        with self.subTest(phase="refuses_a_live_root_below_the_floor"):
            with tempfile.TemporaryDirectory() as tmp:
                tree = Path(tmp) / 'esp-tree'
                esp_tree(tree, bytes(pe_fixture(live_cmdline())))
                with self.assertRaisesRegex(OSError, '40 GiB floor'):
                    self.export(tmp, FakeSystem(tree), LIVE_ROOT_FLOOR=16 << 20)
        with self.subTest(phase="refuses_without_privileges_before_creating_anything"):
            with tempfile.TemporaryDirectory() as tmp:
                image, output = Path(tmp) / 'live.raw', Path(tmp) / 'bundle'
                live_disk(image)
                with patch.object(bundle.os, 'geteuid', return_value=1000):
                    with self.assertRaisesRegex(PermissionError, 'needs (root|Linux)'):
                        bundle.export_image(image, output, 'fixture-only', 'x86_64')
                self.assertEqual(sorted(p.name for p in Path(tmp).iterdir()), ['live.raw'])


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


class FailureTests(unittest.TestCase):
    def test_failed_export_retains_owned_output(self):
        with self.subTest(phase="post_copy_failure_reports_and_retains_owned_output"):
            with tempfile.TemporaryDirectory() as tmp:
                image, output = Path(tmp) / 'fixture.raw', Path(tmp) / 'bundle'
                gpt_fixture(image)
                original = bundle.copy_extent
                def fail_after_copy(*args, **kwargs):
                    original(*args, **kwargs)
                    raise OSError('injected file-copy failure')
                with patch.object(bundle, 'copy_extent', fail_after_copy):
                    with self.assertRaisesRegex(OSError, 'partial output retained.*count=1'):
                        bundle.export_ranges(image, output)
                self.assertEqual((output / 'staging-esp.img').read_bytes(), b'E'*4096)
                self.assertFalse((output / 'manifest.json').exists())
                self.assertTrue(image.exists())
        restart_fixture(self)
        with self.subTest(phase="partial_owned_write_is_retained_with_truthful_error"):
            with tempfile.TemporaryDirectory() as tmp:
                image, output = Path(tmp) / 'fixture.raw', Path(tmp) / 'bundle'
                gpt_fixture(image)
                original = bundle.copy_extent
                class FailingReader:
                    def __init__(self, stream):
                        self.stream, self.reads = stream, 0
                    def seek(self, offset):
                        return self.stream.seek(offset)
                    def read(self, size):
                        self.reads += 1
                        if self.reads > 1:
                            raise OSError('injected read failure after 1024 bytes')
                        return self.stream.read(min(size, 1024))
                def fail_partway(stream, *args, **kwargs):
                    return original(FailingReader(stream), *args, **kwargs)
                with patch.object(bundle, 'copy_extent', fail_partway):
                    with self.assertRaisesRegex(OSError, 'partial output retained.*count=1'):
                        bundle.export_ranges(image, output)
                self.assertEqual((output / 'staging-esp.img').read_bytes(), b'E'*1024)
                self.assertFalse((output / 'staging-root.img').exists())
                self.assertFalse((output / 'manifest.json').exists())
        restart_fixture(self)
        with self.subTest(phase="existing_output_symlink_and_directory_are_preserved"):
            with tempfile.TemporaryDirectory() as tmp:
                image, output = Path(tmp) / 'fixture.raw', Path(tmp) / 'bundle'
                gpt_fixture(image)
                output.symlink_to(image)
                with self.assertRaises(FileExistsError): bundle.export_ranges(image, output)
                with self.assertRaises(FileExistsError):
                    bundle.export_image(image, output, 'fixture-only', 'x86_64', system=FakeSystem(Path(tmp)))
                self.assertTrue(output.is_symlink())
                output.unlink()
                output.mkdir()
                (output / 'sentinel').write_bytes(b'keep')
                with self.assertRaises(FileExistsError): bundle.export_ranges(image, output)
                with self.assertRaises(FileExistsError):
                    bundle.export_image(image, output, 'fixture-only', 'x86_64', system=FakeSystem(Path(tmp)))
                self.assertEqual((output / 'sentinel').read_bytes(), b'keep')


class OwnershipTests(unittest.TestCase):
    def test_replaced_output_is_not_deleted_on_failure(self):
        with tempfile.TemporaryDirectory() as tmp:
            image, output = Path(tmp) / 'fixture.raw', Path(tmp) / 'bundle'
            moved = Path(tmp) / 'moved-owned-bundle'
            gpt_fixture(image)
            original = bundle.copy_extent
            def replace_after_copy(*args, **kwargs):
                original(*args, **kwargs)
                output.rename(moved)
                output.mkdir()
                (output / 'sentinel').write_bytes(b'unrelated')
                raise OSError('injected replacement')
            with patch.object(bundle, 'copy_extent', replace_after_copy):
                with self.assertRaises(OSError): bundle.export_ranges(image, output)
            self.assertTrue((output / 'sentinel').exists())
            self.assertEqual((output / 'sentinel').read_bytes(), b'unrelated')


def qcow_fixture():
    data = bytearray(104)
    struct.pack_into('>IIQIIQIIQQIIQ', data, 0, 0x514649fb, 3, 0, 0, 16, 65536, 0,
                     1, 65536, 131072, 1, 0, 0)
    struct.pack_into('>QQQII', data, 72, 0, 0, 0, 4, 104)
    return data


class QcowTests(unittest.TestCase):
    def test_qcow2_header_parsing_and_triage(self):
        with self.subTest(phase="reads_bounded_self_contained_header"):
            self.assertEqual(bundle.qcow_virtual_size(bytes(qcow_fixture())), 65536)
        with self.subTest(phase="reads_the_112_byte_header_current_qemu_writes"):
            data = qcow_fixture() + bytearray(8)
            struct.pack_into('>I', data, 100, 112)
            self.assertEqual(bundle.qcow_virtual_size(bytes(data)), 65536)
            for problem in ('zstd', 'padding', 'truncated'):
                with self.subTest(problem=problem):
                    changed = bytearray(data)
                    if problem == 'zstd': changed[104] = 1
                    if problem == 'padding': changed[111] = 1
                    if problem == 'truncated': changed = changed[:108]
                    with self.assertRaises(ValueError): bundle.qcow_virtual_size(bytes(changed))
        with self.subTest(phase="rejects_external_backing_features_encryption_snapshots_and_oversize"):
            changes = [(8, '>Q', 1024), (16, '>I', 9), (72, '>Q', 4),
                       (32, '>I', 1), (60, '>I', 1), (24, '>Q', 1<<41), (20, '>I', 31)]
            for offset, fmt, value in changes:
                with self.subTest(offset=offset):
                    data = qcow_fixture()
                    struct.pack_into(fmt, data, offset, value)
                    with self.assertRaises(ValueError): bundle.qcow_virtual_size(bytes(data))
        with self.subTest(phase="qcow2_export_triage_runs_before_privileges"):
            with tempfile.TemporaryDirectory() as tmp:
                image, output = Path(tmp) / 'live.qcow2', Path(tmp) / 'bundle'
                data = qcow_fixture()
                struct.pack_into('>Q', data, 8, 1024)
                image.write_bytes(data)
                system = FakeSystem(Path(tmp))
                with self.assertRaisesRegex(ValueError, 'backing'):
                    bundle.export_image(image, output, 'fixture-only', 'x86_64', system=system)
                self.assertEqual(system.calls, [])
                self.assertFalse(output.exists())


class CliTests(unittest.TestCase):
    def test_cli_export_and_inspect_reports(self):
        with self.subTest(phase="export_without_privileges_reports_why_and_creates_nothing"):
            import contextlib
            import io
            with tempfile.TemporaryDirectory() as tmp:
                image, output = Path(tmp) / 'fixture.qcow2', Path(tmp) / 'bundle'
                image.write_bytes(qcow_fixture())
                errors = io.StringIO()
                with patch.object(bundle.os, 'geteuid', return_value=1000), contextlib.redirect_stderr(errors):
                    result = bundle.main(['export', '--image', str(image), '--arch', 'x86_64',
                                          '--output', str(output), '--release', 'fixture-only'])
                self.assertEqual(result, 2)
                self.assertRegex(errors.getvalue(), 'needs (root|Linux)')
                self.assertEqual(sorted(p.name for p in Path(tmp).iterdir()), ['fixture.qcow2'])
        with self.subTest(phase="inspect_reports_only_schema_and_component_verification"):
            import contextlib
            import io
            with tempfile.TemporaryDirectory() as tmp:
                image, output = Path(tmp) / 'fixture.raw', Path(tmp) / 'bundle'
                gpt_fixture(image)
                manifest = fixture_manifest(image, output)
                path = output / 'manifest.json'
                path.write_text(json.dumps(manifest))
                result = io.StringIO()
                with contextlib.redirect_stdout(result):
                    self.assertEqual(bundle.main(['inspect', '--manifest', str(path)]), 0)
                self.assertIn('bootability_not_established', result.getvalue())


class ProfileTests(unittest.TestCase):
    def test_rejects_read_only_physical_root_and_wrong_filesystem(self):
        valid = valid_fixture()
        invalid = copy.deepcopy(valid)
        invalid['boot_binding']['uki_cmdline'] = invalid['boot_binding']['uki_cmdline'].replace('rw ', 'ro ')
        with self.assertRaises(ValueError): bundle.validate_manifest(invalid)
        invalid = copy.deepcopy(valid)
        invalid['components'][1]['filesystem_type'] = 'ext4'
        with self.assertRaises(ValueError): bundle.validate_manifest(invalid)


class CorrectionTests(unittest.TestCase):
    def test_concurrent_substitution_is_preserved(self):
        with self.subTest(phase="next_component_exclusive_open_preserves_unrelated_sentinel"):
            with tempfile.TemporaryDirectory() as tmp:
                image, output = Path(tmp) / 'fixture.raw', Path(tmp) / 'bundle'
                gpt_fixture(image)
                original = bundle.copy_extent
                def introduce_collision(*args, **kwargs):
                    result = original(*args, **kwargs)
                    (output / 'staging-root.img').write_bytes(b'unrelated')
                    return result
                with patch.object(bundle, 'copy_extent', introduce_collision):
                    with self.assertRaises(OSError): bundle.export_ranges(image, output)
                self.assertTrue((output / 'staging-root.img').exists())
                self.assertEqual((output / 'staging-root.img').read_bytes(), b'unrelated')
        with self.subTest(phase="directory_substituted_before_open_is_preserved"):
            with tempfile.TemporaryDirectory() as tmp:
                image, output = Path(tmp) / 'fixture.raw', Path(tmp) / 'bundle'
                moved = Path(tmp) / 'original-directory'
                gpt_fixture(image)
                original_open = os.open
                def replace_before_open(path, flags, *args, **kwargs):
                    if Path(path) == output and flags & os.O_DIRECTORY:
                        output.rename(moved)
                        output.mkdir(mode=0o700)
                        (output / 'staging-root.img').write_bytes(b'unrelated')
                    return original_open(path, flags, *args, **kwargs)
                with patch.object(bundle.os, 'open', replace_before_open):
                    with self.assertRaises((OSError, ValueError)): bundle.export_ranges(image, output)
                self.assertTrue((output / 'staging-root.img').exists())
                self.assertEqual((output / 'staging-root.img').read_bytes(), b'unrelated')
        with self.subTest(phase="substitution_before_first_stat_preserves_nonempty_replacement"):
            with tempfile.TemporaryDirectory() as tmp:
                image, output = Path(tmp) / 'fixture.raw', Path(tmp) / 'bundle'
                moved = Path(tmp) / 'original-directory'
                gpt_fixture(image)
                original_stat = Path.stat
                substituted = False
                def replace_before_stat(path, *args, **kwargs):
                    nonlocal substituted
                    if path == output and not substituted:
                        substituted = True
                        output.rename(moved)
                        output.mkdir(mode=0o700)
                        (output / 'sentinel').write_bytes(b'unrelated')
                    return original_stat(path, *args, **kwargs)
                with patch.object(bundle.Path, 'stat', replace_before_stat):
                    with self.assertRaisesRegex(OSError, 'partial output retained.*count=0'):
                        bundle.export_ranges(image, output)
                self.assertEqual((output / 'sentinel').read_bytes(), b'unrelated')
                self.assertFalse((output / 'staging-esp.img').exists())
        with self.subTest(phase="later_owned_filename_substitution_is_preserved"):
            with tempfile.TemporaryDirectory() as tmp:
                image, output = Path(tmp) / 'fixture.raw', Path(tmp) / 'bundle'
                gpt_fixture(image)
                original = bundle.copy_extent
                def substitute_after_copy(*args, **kwargs):
                    original(*args, **kwargs)
                    (output / 'staging-esp.img').unlink()
                    (output / 'staging-esp.img').write_bytes(b'unrelated-replacement')
                    raise OSError('injected failure after file substitution')
                with patch.object(bundle, 'copy_extent', substitute_after_copy):
                    with self.assertRaises(OSError): bundle.export_ranges(image, output)
                self.assertTrue((output / 'staging-esp.img').exists())
                self.assertEqual((output / 'staging-esp.img').read_bytes(), b'unrelated-replacement')
        with self.subTest(phase="later_replacement_is_detected_without_injected_failure"):
            with tempfile.TemporaryDirectory() as tmp:
                image, output = Path(tmp) / 'fixture.raw', Path(tmp) / 'bundle'
                gpt_fixture(image)
                original = bundle.copy_extent
                def substitute_after_copy(stream, filename, *args, **kwargs):
                    result = original(stream, filename, *args, **kwargs)
                    if filename == 'staging-esp.img':
                        (output / filename).rename(output / 'moved-original.img')
                        (output / filename).write_bytes(b'X'*4096)
                    return result
                with patch.object(bundle, 'copy_extent', substitute_after_copy):
                    with self.assertRaisesRegex(OSError, 'partial output retained.*component replaced'):
                        bundle.export_ranges(image, output)
                self.assertEqual((output / 'staging-esp.img').read_bytes(), b'X'*4096)

    def test_gpt_attribute_handling(self):
        with self.subTest(phase="grow_root_attributes_are_retained_by_structural_export"):
            with tempfile.TemporaryDirectory() as tmp:
                image = Path(tmp) / 'fixture.raw'
                for i, kind in enumerate(('4f68bce3-e8cd-4db1-96e7-fbcaf984b709',
                                          'b921b045-1df0-41c3-af44-4c6f280d3fae', TILO_ROOT)):
                    gpt_fixture(image, root_type=kind, root_attributes=1<<59)
                    try:
                        partitions = bundle.read_gpt(image)
                        components = bundle.export_ranges(image, Path(tmp) / ('bundle'+str(i)))
                    except ValueError as error:
                        self.fail('producer grow-filesystem fixture rejected: ' + str(error))
                    self.assertEqual(partitions[1].get('attributes'), 1<<59)
                    self.assertEqual(components[1].get('source_attributes'), 1<<59)
                    self.assertEqual(components[0].get('source_attributes'), 0)
        with self.subTest(phase="unsupported_gpt_attribute_combinations_are_refused"):
            with tempfile.TemporaryDirectory() as tmp:
                image = Path(tmp) / 'fixture.raw'
                for options in ({'esp_attributes': 1<<59}, {'root_attributes': 1<<59},
                                {'root_type': '4f68bce3-e8cd-4db1-96e7-fbcaf984b709', 'root_attributes': 1},
                                {'root_type': '4f68bce3-e8cd-4db1-96e7-fbcaf984b709', 'root_attributes': (1<<59)|1},
                                {'root_type': TILO_ROOT, 'root_attributes': 1}):
                    with self.subTest(options=options):
                        gpt_fixture(image, **options)
                        with self.assertRaises(ValueError): bundle.read_gpt(image)


if __name__ == '__main__':
    unittest.main()
