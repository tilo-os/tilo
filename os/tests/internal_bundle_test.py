"""Host-only binary fixtures; these are not bootable/filesystem-qualified images."""
import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import struct
import tempfile
import unittest
import uuid
import zlib

SPEC = importlib.util.spec_from_file_location('internal_bundle', Path(__file__).parents[1] / 'dev/internal-bundle.py')
bundle = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(bundle)
ESP = 'c12a7328-f81f-11d2-ba4b-00a0c93ec93b'
ROOT = '0fc63daf-8483-4772-8e79-3d69d8477de4'
ROOT_UUID = '12345678-1234-4234-8234-123456789abc'


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


def evidence():
    return {'architecture': 'x86_64', 'efi_path': '/EFI/Linux/fixture.efi',
            'uki_architecture': 'x86_64', 'uki_sha256': 'a'*64,
            'uki_cmdline': 'root=UUID=' + ROOT_UUID + ' rw qos.install composefs=' + 'b'*128,
            'root_locator': 'UUID=' + ROOT_UUID,
            'filesystems': {'staging-esp': {'type': 'vfat', 'uuid': '1234-ABCD'},
                            'staging-root': {'type': 'btrfs', 'uuid': ROOT_UUID}}}


def fixture_manifest(image, output):
    components = bundle.export_ranges(image, output)
    fs = evidence()['filesystems']
    for component in components:
        # Structural source attributes are not staging flags or a schema expansion.
        component.pop('source_attributes', None)
        component['filesystem_type'] = fs[component['role']]['type']
        component['filesystem_uuid'] = fs[component['role']]['uuid']
    boot = evidence()
    boot.pop('filesystems')
    architecture = boot.pop('architecture')
    boot['composefs_digest'] = 'b'*128
    return {'schema': 1, 'purpose': 'qos-internal-installer', 'release_id': 'fixture-only',
            'architecture': architecture, 'firmware': 'uefi', 'secure_boot': 'disabled',
            'installer_protocol': 1, 'source_image_bytes': image.stat().st_size,
            'components': components, 'boot_binding': boot}


def valid_fixture():
    with tempfile.TemporaryDirectory() as tmp:
        image = Path(tmp) / 'fixture.raw'
        gpt_fixture(image)
        manifest = fixture_manifest(image, Path(tmp) / 'bundle')
    bundle.validate_manifest(manifest)
    return manifest


class GptTests(unittest.TestCase):
    def test_reads_exact_partition_extents(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'fixture.raw'
            gpt_fixture(path)
            self.assertEqual([(p['offset'], p['size']) for p in bundle.read_gpt(path)],
                             [(17408, 4096), (21504, 8192)])

    def test_rejects_overlap_crc_damage_and_truncation(self):
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


class ExportTests(unittest.TestCase):
    def test_exports_exact_bytes_and_validates_fixture(self):
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

    def test_failed_export_leaves_no_manifest_or_output(self):
        with tempfile.TemporaryDirectory() as tmp:
            image, output = Path(tmp) / 'fixture.raw', Path(tmp) / 'bundle'
            gpt_fixture(image)
            gpt_fixture(image, overlap=True)
            with self.assertRaises(ValueError): bundle.export_ranges(image, output)
            self.assertFalse(output.exists())

    def test_inspection_rejects_changed_bytes_and_symlinks(self):
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
    def test_invalid_single_fields(self):
        baseline = valid_fixture()
        mutations = [
            ('schema', lambda m: m.update(schema=True)),
            ('unknown', lambda m: m.update(extra=1)),
            ('release bound', lambda m: m.update(release_id='x'*129)),
            ('duplicate roles', lambda m: m['components'][1].update(role='staging-esp')),
            ('unsupported role', lambda m: m['components'][1].update(role='target-root')),
            ('overlap', lambda m: m['components'][1].update(source_offset=17408)),
            ('traversal', lambda m: m['components'][0].update(filename='../outside.img')),
            ('zero', lambda m: m['components'][0].update(byte_size=0)),
            ('too large', lambda m: m['components'][0].update(byte_size=1<<63)),
            ('composefs mismatch', lambda m: m['boot_binding'].update(composefs_digest='c'*128)),
            ('root UUID', lambda m: m['components'][1].update(filesystem_uuid=str(uuid.UUID(int=9)))),
            ('architecture', lambda m: m.update(architecture='aarch64')),
            ('unknown binding', lambda m: m['boot_binding'].update(uki_cmdline='composefs=abc qos.install')),
            ('duplicate root', lambda m: m['boot_binding'].update(uki_cmdline=m['boot_binding']['uki_cmdline']+' root=UUID='+ROOT_UUID)),
            ('placeholder', lambda m: m['boot_binding'].update(uki_cmdline='qos.root='+'0'*37+' qos.install')),
            ('efi escape', lambda m: m['boot_binding'].update(efi_path='/EFI/../evil.efi')),
        ]
        for name, change in mutations:
            with self.subTest(name=name):
                manifest = copy.deepcopy(baseline)
                change(manifest)
                with self.assertRaises(ValueError): bundle.validate_manifest(manifest)

    def test_strict_bounded_json(self):
        for raw in (b'{"schema":1,"schema":1}', b'{"x":NaN}', b'['*100+b']'*100, b' '*65537):
            with self.subTest(raw=raw[:30]):
                with self.assertRaises(ValueError): bundle.parse_manifest(raw)
        manifest = valid_fixture()
        self.assertEqual(bundle.parse_manifest(json.dumps(manifest).encode()), manifest)


def pe_fixture(cmdline=None, machine=0x8664, profile=False):
    """Structurally meaningful PE fixture, not executable kernel/initrd content."""
    cmdline = cmdline or evidence()['uki_cmdline']
    sections = [(b'.linux', b'fixture kernel'), (b'.initrd', b'fixture initrd'),
                (b'.osrel', b'ID=qos\n'), (b'.cmdline', cmdline.encode()+b'\0')]
    if profile:
        sections.append((b'.profile', b'ID=fixture-profile\n'))
    data = bytearray(4096)
    data[:2] = b'MZ'
    struct.pack_into('<I', data, 0x3c, 128)
    data[128:132] = b'PE\0\0'
    struct.pack_into('<HH', data, 132, machine, len(sections))
    struct.pack_into('<HH', data, 148, 240, 0x2022)
    struct.pack_into('<H', data, 152, 0x20b)
    struct.pack_into('<H', data, 152+68, 10)
    for i, (name, payload) in enumerate(sections):
        offset = 1024 + i*512
        struct.pack_into('<8sIIII', data, 392+i*40, name, len(payload), (i+1)*4096, 512, offset)
        data[offset:offset+len(payload)] = payload
    return data


class UkiTests(unittest.TestCase):
    def test_measures_machine_and_exact_cmdline(self):
        info = bundle.inspect_uki(bytes(pe_fixture()))
        self.assertEqual(info.get('uki_architecture'), 'x86_64')
        self.assertEqual(info['uki_cmdline'], evidence()['uki_cmdline'])
        self.assertEqual(info['uki_sha256'], hashlib.sha256(pe_fixture()).hexdigest())

    def test_rejects_ambiguous_sections_and_truncation(self):
        for problem in ('duplicate', 'overlap', 'truncated', 'wrong machine', 'embedded nul'):
            with self.subTest(problem=problem):
                data = pe_fixture()
                if problem == 'duplicate': data[392:400] = b'.cmdline'
                if problem == 'overlap': struct.pack_into('<I', data, 432+20, 1024)
                if problem == 'truncated': data = data[:2000]
                if problem == 'wrong machine': struct.pack_into('<H', data, 132, 0x14c)
                if problem == 'embedded nul': data = pe_fixture('root=UUID='+ROOT_UUID+'\0 qos.install')
                with self.assertRaises(ValueError): bundle.inspect_uki(bytes(data))


class FailureTests(unittest.TestCase):
    def test_post_copy_failure_reports_and_retains_owned_output(self):
        from unittest.mock import patch
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

    def test_partial_owned_write_is_retained_with_truthful_error(self):
        from unittest.mock import patch
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

    def test_existing_output_symlink_and_directory_are_preserved(self):
        with tempfile.TemporaryDirectory() as tmp:
            image, output = Path(tmp) / 'fixture.raw', Path(tmp) / 'bundle'
            gpt_fixture(image)
            output.symlink_to(image)
            with self.assertRaises(FileExistsError): bundle.export_ranges(image, output)
            self.assertTrue(output.is_symlink())
            output.unlink()
            output.mkdir()
            (output / 'sentinel').write_bytes(b'keep')
            with self.assertRaises(FileExistsError): bundle.export_ranges(image, output)
            self.assertEqual((output / 'sentinel').read_bytes(), b'keep')


class OwnershipTests(unittest.TestCase):
    def test_replaced_output_is_not_deleted_on_failure(self):
        from unittest.mock import patch
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


class SpecializationTests(unittest.TestCase):
    def test_specializes_only_root_slot_and_preserves_composefs(self):
        original_cmdline = ('rw qos.mode=normal qos.root=' + '0'*37 +
                            ' qos.luks=' + '0'*43 + ' qos.unlock=' + '0'*74 +
                            ' composefs=' + 'b'*128 + ' qos.install')
        original = bytes(pe_fixture(original_cmdline))
        changed = bundle.specialize_uki(original, ROOT_UUID)
        expected = original.replace(('qos.root='+'0'*37).encode(), ('root=UUID='+ROOT_UUID).encode())
        self.assertEqual(changed, expected)
        info = bundle.inspect_uki(changed)
        bundle.validate_cmdline(info['uki_cmdline'], 'UUID='+ROOT_UUID, 'b'*128)

    def test_rejects_unrecognized_or_conflicting_producer_binding(self):
        for cmdline in ('qos.root='+'0'*37+' qos.install',
                        'root=LABEL=root composefs='+'b'*128+' qos.install',
                        'rw qos.root='+'0'*37+' composefs='+'b'*128+' qos.install rootflags=subvol=other'):
            with self.subTest(cmdline=cmdline):
                with self.assertRaises(ValueError): bundle.specialize_uki(bytes(pe_fixture(cmdline)), ROOT_UUID)


def qcow_fixture():
    data = bytearray(104)
    struct.pack_into('>IIQIIQIIQQIIQ', data, 0, 0x514649fb, 3, 0, 0, 16, 65536, 0,
                     1, 65536, 131072, 1, 0, 0)
    struct.pack_into('>QQQII', data, 72, 0, 0, 0, 4, 104)
    return data


class QcowTests(unittest.TestCase):
    def test_reads_bounded_self_contained_header(self):
        self.assertEqual(bundle.qcow_virtual_size(bytes(qcow_fixture())), 65536)

    def test_rejects_external_backing_features_encryption_snapshots_and_oversize(self):
        changes = [(8, '>Q', 1024), (16, '>I', 9), (72, '>Q', 4),
                   (32, '>I', 1), (60, '>I', 1), (24, '>Q', 1<<41), (20, '>I', 31)]
        for offset, fmt, value in changes:
            with self.subTest(offset=offset):
                data = qcow_fixture()
                struct.pack_into(fmt, data, offset, value)
                with self.assertRaises(ValueError): bundle.qcow_virtual_size(bytes(data))


class CliTests(unittest.TestCase):
    def test_export_reports_specific_unqualified_gate_without_output(self):
        import contextlib
        import io
        with tempfile.TemporaryDirectory() as tmp:
            image, output = Path(tmp) / 'fixture.qcow2', Path(tmp) / 'bundle'
            image.write_bytes(qcow_fixture())
            errors = io.StringIO()
            with contextlib.redirect_stderr(errors):
                result = bundle.main(['export', '--image', str(image), '--output', str(output), '--release', 'fixture-only'])
            self.assertEqual(result, 2)
            self.assertIn('artifact-bound consumer', errors.getvalue())
            self.assertFalse(output.exists())

    def test_inspect_reports_only_schema_and_component_verification(self):
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
        invalid['boot_binding']['uki_cmdline'] = invalid['boot_binding']['uki_cmdline'].replace(' rw ', ' ro ')
        with self.assertRaises(ValueError): bundle.validate_manifest(invalid)
        invalid = copy.deepcopy(valid)
        invalid['components'][1]['filesystem_type'] = 'ext4'
        with self.assertRaises(ValueError): bundle.validate_manifest(invalid)

    def test_specialization_rejects_signed_pe(self):
        cmdline = 'rw qos.root='+'0'*37+' composefs='+'b'*128+' qos.install'
        data = pe_fixture(cmdline)
        struct.pack_into('<I', data, 152+108, 16)
        struct.pack_into('<II', data, 152+112+4*8, 3500, 100)
        with self.assertRaises(ValueError): bundle.specialize_uki(bytes(data), ROOT_UUID)


class CorrectionTests(unittest.TestCase):
    def test_next_component_exclusive_open_preserves_unrelated_sentinel(self):
        from unittest.mock import patch
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

    def test_directory_substituted_before_open_is_preserved(self):
        from unittest.mock import patch
        import os
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

    def test_substitution_before_first_stat_preserves_nonempty_replacement(self):
        from unittest.mock import patch
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

    def test_later_owned_filename_substitution_is_preserved(self):
        from unittest.mock import patch
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

    def test_later_replacement_is_detected_without_injected_failure(self):
        from unittest.mock import patch
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

    def test_profile_specialization_is_refused(self):
        cmdline = 'rw qos.root='+'0'*37+' composefs='+'b'*128+' qos.install'
        data = bytes(pe_fixture(cmdline, profile=True))
        self.assertEqual(bundle.inspect_uki(data)['uki_cmdline'], cmdline)
        with self.assertRaises(ValueError): bundle.specialize_uki(data, ROOT_UUID)

    def test_grow_root_attributes_are_retained_by_structural_export(self):
        with tempfile.TemporaryDirectory() as tmp:
            image = Path(tmp) / 'fixture.raw'
            for i, kind in enumerate(('4f68bce3-e8cd-4db1-96e7-fbcaf984b709',
                                      'b921b045-1df0-41c3-af44-4c6f280d3fae')):
                gpt_fixture(image, root_type=kind, root_attributes=1<<59)
                try:
                    partitions = bundle.read_gpt(image)
                    components = bundle.export_ranges(image, Path(tmp) / ('bundle'+str(i)))
                except ValueError as error:
                    self.fail('producer grow-filesystem fixture rejected: ' + str(error))
                self.assertEqual(partitions[1].get('attributes'), 1<<59)
                self.assertEqual(components[1].get('source_attributes'), 1<<59)
                self.assertEqual(components[0].get('source_attributes'), 0)

    def test_unsupported_gpt_attribute_combinations_are_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            image = Path(tmp) / 'fixture.raw'
            for options in ({'esp_attributes': 1<<59}, {'root_attributes': 1<<59},
                            {'root_type': '4f68bce3-e8cd-4db1-96e7-fbcaf984b709', 'root_attributes': 1},
                            {'root_type': '4f68bce3-e8cd-4db1-96e7-fbcaf984b709', 'root_attributes': (1<<59)|1}):
                with self.subTest(options=options):
                    gpt_fixture(image, **options)
                    with self.assertRaises(ValueError): bundle.read_gpt(image)


if __name__ == '__main__':
    unittest.main()
