#!/usr/bin/env python3
"""Behavioral tests for source identity and paired Wine loader contracts."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location('wine_contract', Path(__file__).resolve().parents[1] / 'dev/wine_rpm_contract.py')
C = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(C)


def layout(arch):
    own, other, base, alternate, bits = ('x86_64', 'i386', '/usr/lib64/wine', '/usr/lib/wine', 64) if arch == 'x86_64' else ('i386', 'x86_64', '/usr/lib/wine', '/usr/lib64/wine', 32)
    return {f'/usr/bin/wine{bits}': '', f'{base}/{own}-unix/ntdll.so': '',
            f'{base}/{own}-unix/wine': '',
            f'{base}/{other}-unix': f'{alternate}/{other}-unix',
            f'{base}/{other}-windows': f'{alternate}/{other}-windows'}


class ContractTests(unittest.TestCase):
    def test_supported_traditional_layout_both_targets(self):
        for arch in ('i686', 'x86_64'):
            C.validate_core_layout(arch, layout(arch))

    def test_missing_or_misdirected_cross_width_loader_rejected(self):
        for arch in ('i686', 'x86_64'):
            original = layout(arch)
            link = next(name for name, value in original.items() if value and name.endswith('-unix'))
            for value in (None, '/usr/lib/wine-wow64/wine/i386-unix', '/tmp/loader', ''):
                files = original.copy()
                if value is None:
                    del files[link]
                else:
                    files[link] = value
                with self.subTest(arch=arch, target=value), self.assertRaises(ValueError):
                    C.validate_core_layout(arch, files)

    def test_new_wow64_payload_rejected_even_with_added_links(self):
        files = layout('x86_64')
        files['/usr/lib64/wine-wow64/wine/x86_64-unix/ntdll.so'] = ''
        with self.assertRaisesRegex(ValueError, 'new WoW64'):
            C.validate_core_layout('x86_64', files)

    def test_native_loader_cannot_be_a_redirecting_symlink(self):
        files = layout('i686')
        files['/usr/bin/wine32'] = '/tmp/substitute'
        with self.assertRaisesRegex(ValueError, 'regular native loader'):
            C.validate_core_layout('i686', files)

    def test_moving_buildroot_and_foreign_target_rejected(self):
        C.reference('registry.example/wine@sha256:' + 'a' * 64)
        for value in ('registry.example/wine:latest', 'wine@sha256:abc', 'wine@sha256:' + 'G' * 64):
            with self.assertRaises(ValueError):
                C.reference(value)
        with self.assertRaises(ValueError):
            C.target('aarch64')

    def test_source_must_match_bytes_size_and_regular_file(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'source.rpm'
            path.write_bytes(b'verified source fixture')
            expected = C.sha(path)
            C.verify_file(path, expected, path.stat().st_size)
            with self.assertRaises(ValueError):
                C.verify_file(path, expected, path.stat().st_size + 1)
            path.write_bytes(b'modified source fixture')
            with self.assertRaises(ValueError):
                C.verify_file(path, expected)
            link = path.with_name('link.rpm')
            link.symlink_to(path)
            with self.assertRaises(ValueError):
                C.verify_file(link, C.sha(path))

    def test_empty_core_output_is_not_an_artifact(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            (directory / 'rpms').mkdir()
            with self.assertRaisesRegex(ValueError, 'wine-core'):
                C.output('i686', {}, 'repo@sha256:' + 'a' * 64, directory)

    def test_query_retains_empty_link_field_on_last_file(self):
        with patch.object(C.subprocess, 'check_output', return_value='/loader\t\n'):
            self.assertEqual(C.run(['rpm']), '/loader\t')

    def test_elf_headers_must_match_requested_target(self):
        for arch, bits, machine in (('i686', 1, 3), ('x86_64', 2, 62)):
            header = bytearray(20)
            header[:6] = b'\x7fELF' + bytes([bits, 1])
            header[18:20] = machine.to_bytes(2, 'little')
            self.assertEqual(C.elf_identity(header, arch), (bits, machine))
            with self.assertRaises(ValueError):
                C.elf_identity(header, 'i686' if arch == 'x86_64' else 'x86_64')
            header[18:20] = (183).to_bytes(2, 'little')
            with self.assertRaises(ValueError):
                C.elf_identity(header, arch)


if __name__ == '__main__':
    unittest.main()
