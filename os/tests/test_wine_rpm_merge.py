#!/usr/bin/env python3
"""Noarch merge must preserve package contents, dependencies and scriptlets."""
import importlib.util
from pathlib import Path
import sys
import unittest
from unittest.mock import patch
import xml.etree.ElementTree as ET

DEV = Path(__file__).resolve().parents[1] / 'dev'
sys.path.insert(0, str(DEV))
SPEC = importlib.util.spec_from_file_location('wine_merge', DEV / 'merge-wine-rpms.py')
M = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(M)
sys.path.pop(0)


def header(**changes):
    values = {'Name': 'wine-common', 'Version': '11.0', 'Release': '3.fc44.qos_traditional1',
              'Arch': 'noarch', 'Filenames': '/usr/share/wine/file', 'Filedigests': 'abc',
              'Filemodes': '33188', 'Fileusername': 'root', 'Fileflags': '2',
              'Prein': 'echo preserve-script', 'Requirename': 'wine-core',
              'Requireversion': '11.0-3.fc44.qos_traditional1', 'Triggername': 'wine',
              'Buildhost': 'builder-a', 'Buildtime': '1234', 'Payloadcompressor': 'zstd'}
    values.update(changes)
    root = ET.Element('rpmHeader')
    for name, value in values.items():
        ET.SubElement(ET.SubElement(root, 'rpmTag', name=name), 'string').text = value
    return ET.tostring(root, encoding='unicode')


def package(arch):
    return {'identity': ['wine-common' if arch == 'noarch' else 'wine-core', '11.0',
                         '3.fc44.qos_traditional1', arch, 'wine.src.rpm', 'LGPL'],
            'sha256': arch, 'size': 1}


class MergeTests(unittest.TestCase):
    def test_build_host_encoding_changes_do_not_change_semantic_package(self):
        self.assertEqual(M.semantic_header(header()), M.semantic_header(header(
            Buildhost='builder-b', Buildtime='9999', Payloadcompressor='xz')))

    def test_file_script_dependency_trigger_or_license_changes_reject_merge(self):
        for name in ('Filedigests', 'Filenames', 'Filemodes', 'Fileusername', 'Fileflags',
                     'Prein', 'Requirename', 'Requireversion', 'Triggername', 'License'):
            def run(argv):
                if argv[0] == 'rpmkeys':
                    return 'digests OK'
                return header() if argv[-1] == 'left.rpm' else header(**{name: 'changed'})
            with self.subTest(name=name), patch.object(M.contract, 'run', side_effect=run):
                with self.assertRaisesRegex(ValueError, 'noarch output differs'):
                    M.noarch_equal('left.rpm', 'right.rpm')

    def test_every_architecture_package_is_retained_and_shared_owner_is_explicit(self):
        inventories = [{'common.noarch.rpm': package('noarch'), 'core.x86_64.rpm': package('x86_64')},
                       {'common.noarch.rpm': package('noarch'), 'core.i686.rpm': package('i686')}]
        calls = []
        selected = M.select_packages([Path('/64'), Path('/32')], inventories,
                                     compare=lambda a, b: calls.append((a, b)))
        self.assertEqual(len(selected), 3)
        self.assertEqual(selected['common.noarch.rpm']['owner'], 'x86_64')
        self.assertEqual(selected['core.i686.rpm']['owner'], 'i686')
        self.assertEqual(calls, [(Path('/64/rpms/common.noarch.rpm'), Path('/32/rpms/common.noarch.rpm'))])

    def test_missing_noarch_counterpart_is_not_silently_dropped(self):
        with self.assertRaisesRegex(ValueError, 'complete noarch package set'):
            M.select_packages([Path('/64'), Path('/32')], [{'shared.rpm': package('noarch')}, {}])

    def test_arch_specific_filename_collision_is_not_deduplicated(self):
        with self.assertRaisesRegex(ValueError, 'architecture-specific'):
            M.select_packages([Path('/64'), Path('/32')],
                              [{'core.rpm': package('x86_64')}, {'core.rpm': package('i686')}])

    def test_unknown_semantic_tags_are_compared_by_default(self):
        self.assertNotEqual(M.semantic_header(header()), M.semantic_header(header(FutureSecurityFlag='yes')))

    def test_wrong_arch_or_missing_file_digests_rejected(self):
        with self.assertRaises(ValueError):
            M.semantic_header(header(Arch='x86_64'))
        root = ET.fromstring(header())
        root.remove(next(tag for tag in root if tag.attrib['name'] == 'Filedigests'))
        with self.assertRaises(ValueError):
            M.semantic_header(ET.tostring(root, encoding='unicode'))


if __name__ == '__main__':
    unittest.main()
