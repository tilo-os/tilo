#!/usr/bin/env python3
"""Noarch merge must preserve package contents, dependencies and scriptlets."""
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
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
    values = {'Name': 'wine-common', 'Version': '11.0', 'Release': '3.fc44.tilo_traditional1',
              'Arch': 'noarch', 'Basenames': 'file', 'Dirnames': '/usr/share/wine/',
              'Dirindexes': '0', 'Filedigests': 'abc',
              'Filemodes': '33188', 'Fileusername': 'root', 'Fileflags': '2',
              'Prein': 'echo preserve-script', 'Requirename': 'wine-core',
              'Requireversion': '11.0-3.fc44.tilo_traditional1', 'Triggername': 'wine',
              'Buildhost': 'builder-a', 'Buildtime': '1234', 'Payloadcompressor': 'zstd'}
    values.update(changes)
    root = ET.Element('rpmHeader')
    for name, value in values.items():
        kind = 'integer' if name == 'Dirindexes' else 'string'
        ET.SubElement(ET.SubElement(root, 'rpmTag', name=name), kind).text = value
    return ET.tostring(root, encoding='unicode')


def package(arch):
    return {'identity': ['wine-common' if arch == 'noarch' else 'wine-core', '11.0',
                         '3.fc44.tilo_traditional1', arch, 'wine.src.rpm', 'LGPL'],
            'sha256': arch, 'size': 1}


class MergeTests(unittest.TestCase):
    def test_provenance_mode_revalidation(self):
        with self.subTest(phase="merge_revalidates_each_explicit_provenance_mode"):
            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                inputs = [root / 'x64', root / 'i686']
                ref = 'repo@sha256:' + 'a' * 64
                records = [{'buildroot_ref': ref, 'rpms': {}},
                           {'producer': {'mode': 'mock-hermetic'}, 'rpms': {}}]
                for directory, record in zip(inputs, records):
                    (directory / 'provenance').mkdir(parents=True)
                    (directory / 'provenance/contract.json').write_text(json.dumps(record))
                with patch.object(M.contract, 'output', side_effect=records) as validate:
                    M.merge(*inputs, root / 'merged', {}, [ref, ref])
                first, second = validate.call_args_list
                self.assertEqual(first.args[2], ref)
                self.assertNotIn('mode', first.kwargs)
                self.assertIsNone(second.args[2])
                self.assertEqual(second.kwargs, {'write': False, 'mode': 'mock-hermetic'})
        with self.subTest(phase="merge_rejects_unknown_or_ambiguous_mode_before_output"):
            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                (root / 'provenance').mkdir()
                ref = 'repo@sha256:' + 'a' * 64
                for record in ({'producer': {'mode': 'unknown'}},
                               {'producer': {'mode': 'mock-hermetic'}, 'buildroot_ref': ref}):
                    (root / 'provenance/contract.json').write_text(json.dumps(record))
                    with patch.object(M.contract, 'output') as validate, self.assertRaisesRegex(ValueError, 'provenance mode'):
                        M.merge(root, root, root / 'merged', {}, [ref, ref])
                    validate.assert_not_called()
                    self.assertFalse((root / 'merged').exists())
        with self.subTest(phase="merge_rejects_revalidated_inventory_drift"):
            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                (root / 'provenance').mkdir()
                (root / 'provenance/contract.json').write_text(json.dumps({'producer': {'mode': 'mock-hermetic'}}))
                ref = 'repo@sha256:' + 'a' * 64
                with patch.object(M.contract, 'output', return_value={'changed': True}), self.assertRaisesRegex(ValueError, 'does not match'):
                    M.merge(root, root, root / 'merged', {}, [ref, ref])
                self.assertFalse((root / 'merged').exists())

    def test_semantic_package_comparison(self):
        with self.subTest(phase="build_host_encoding_changes_do_not_change_semantic_package"):
            self.assertEqual(M.semantic_header(header()), M.semantic_header(header(
                Buildhost='builder-b', Buildtime='9999', Payloadcompressor='xz')))
        with self.subTest(phase="native_target_bookkeeping_does_not_change_noarch_payload"):
            self.assertEqual(
                M.semantic_header(header(Optflags='-O2 -m64', Platform='x86_64-redhat-linux')),
                M.semantic_header(header(Optflags='-O2 -m32', Platform='i686-redhat-linux')),
            )
            self.assertNotEqual(
                M.semantic_header(header(Optflags='-O2 -m64', Filedigests='abc')),
                M.semantic_header(header(Optflags='-O2 -m32', Filedigests='changed')),
            )
        with self.subTest(phase="file_script_dependency_trigger_or_license_changes_reject_merge"):
            for name in ('Filedigests', 'Basenames', 'Filemodes', 'Fileusername', 'Fileflags',
                         'Prein', 'Requirename', 'Requireversion', 'Triggername', 'License'):
                def run(argv):
                    if argv[0] == 'rpmkeys':
                        return 'digests OK'
                    return header() if argv[-1] == 'left.rpm' else header(**{name: 'changed'})
                with self.subTest(name=name), patch.object(M.contract, 'run', side_effect=run):
                    with self.assertRaisesRegex(ValueError, 'noarch output differs'):
                        M.noarch_equal('left.rpm', 'right.rpm')
        with self.subTest(phase="unknown_semantic_tags_are_compared_by_default"):
            self.assertNotEqual(M.semantic_header(header()), M.semantic_header(header(FutureSecurityFlag='yes')))
        with self.subTest(phase="wrong_arch_or_missing_file_digests_rejected"):
            with self.assertRaises(ValueError):
                M.semantic_header(header(Arch='x86_64'))
            root = ET.fromstring(header())
            root.remove(next(tag for tag in root if tag.attrib['name'] == 'Filedigests'))
            with self.assertRaises(ValueError):
                M.semantic_header(ET.tostring(root, encoding='unicode'))

    def test_package_retention(self):
        with self.subTest(phase="every_architecture_package_is_retained_and_shared_owner_is_explicit"):
            inventories = [{'common.noarch.rpm': package('noarch'), 'core.x86_64.rpm': package('x86_64')},
                           {'common.noarch.rpm': package('noarch'), 'core.i686.rpm': package('i686')}]
            calls = []
            selected = M.select_packages([Path('/64'), Path('/32')], inventories,
                                         compare=lambda a, b: calls.append((a, b)))
            self.assertEqual(len(selected), 3)
            self.assertEqual(selected['common.noarch.rpm']['owner'], 'x86_64')
            self.assertEqual(selected['core.i686.rpm']['owner'], 'i686')
            self.assertEqual(calls, [(Path('/64/rpms/common.noarch.rpm'), Path('/32/rpms/common.noarch.rpm'))])
        with self.subTest(phase="missing_noarch_counterpart_is_not_silently_dropped"):
            with self.assertRaisesRegex(ValueError, 'complete noarch package set'):
                M.select_packages([Path('/64'), Path('/32')], [{'shared.rpm': package('noarch')}, {}])
        with self.subTest(phase="arch_specific_filename_collision_is_not_deduplicated"):
            with self.assertRaisesRegex(ValueError, 'architecture-specific'):
                M.select_packages([Path('/64'), Path('/32')],
                                  [{'core.rpm': package('x86_64')}, {'core.rpm': package('i686')}])

    def test_rpm_header_tables(self):
        with self.subTest(phase="actual_installed_rpm_xml_uses_split_filename_table"):
            fixtures = Path(__file__).with_name('fixtures')
            actual = M.semantic_header((fixtures / 'rpm-nano-default-editor.xml').read_text())
            self.assertNotIn('FILENAMES', actual)
            self.assertEqual(len(actual['BASENAMES']), 5)
            self.assertEqual(len(actual['DIRINDEXES']), 5)
            self.assertEqual(len(actual['FILEDIGESTS']), 5)
        with self.subTest(phase="actual_payload_free_metapackage_is_preserved"):
            actual = M.semantic_header((Path(__file__).with_name('fixtures') / 'rpm-default-editor-empty.xml').read_text())
            self.assertEqual(actual['NAME'], [('string', 'default-editor')])
            self.assertNotIn('BASENAMES', actual)
            self.assertIn('REQUIRENAME', actual)
        with self.subTest(phase="directory_indices_and_per_file_array_lengths_are_validated"):
            for index in ('-1', '1', 'garbage'):
                with self.subTest(index=index), self.assertRaisesRegex(ValueError, 'directory index'):
                    M.semantic_header(header(Dirindexes=index))
            for name in ('Dirindexes', 'Filedigests', 'Filemodes', 'Fileusername'):
                root = ET.fromstring(header())
                tag = next(tag for tag in root if tag.attrib['name'] == name)
                ET.SubElement(tag, 'string').text = 'extra'
                with self.subTest(tag=name), self.assertRaisesRegex(ValueError, 'length mismatch'):
                    M.semantic_header(ET.tostring(root, encoding='unicode'))
        with self.subTest(phase="partial_table_cannot_masquerade_as_empty_metapackage"):
            root = ET.fromstring(header())
            for tag in list(root):
                if tag.attrib['name'] in ('Basenames', 'Dirnames', 'Dirindexes'):
                    root.remove(tag)
            with self.assertRaisesRegex(ValueError, 'file table is missing'):
                M.semantic_header(ET.tostring(root, encoding='unicode'))


if __name__ == '__main__':
    unittest.main()
