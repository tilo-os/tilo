import copy
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'dist/macos'))
import collection_runtime as runtime


def info(loads=(), rpaths=(), minimum='13.0'):
    return {'bytes': 4, 'sha256': 'fixture', 'slices': {'arm64': {
        'minimum_macos': minimum, 'install_id': None, 'filetype': 'DYLIB',
        'rpaths': list(rpaths), 'dependencies': [{'load': p, 'kind': 'LC_LOAD_DYLIB'} for p in loads]}}}


class AuditTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='runtime spaces & $() ')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.prefix = self.root / 'dependency root/runtime'
        (self.prefix / 'lib/gstreamer-1.0').mkdir(parents=True)
        self.scanner = self.file(self.prefix / 'libexec/gstreamer-1.0/gst-plugin-scanner')
        self.binary = self.file(self.root / 'App.app/Contents/MacOS/helper')
        self.config = runtime.AuditConfig(self.binary, self.prefix, (self.root / 'dependency root',))
        self.images = {}

    def file(self, path):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(bytes.fromhex('cffaedfe'))
        return path

    def audit(self, config=None):
        with patch.object(runtime, 'image_info', side_effect=lambda path, *_: copy.deepcopy(self.images.get(path, info()))):
            return runtime.audit_runtime(config or self.config)

    def test_resolved_static_graph_never_qualifies_runtime_or_license(self):
        before = sorted(str(p.relative_to(self.root)) for p in self.root.rglob('*'))
        report = self.audit()
        self.assertEqual(report['static_closure']['status'], 'resolved static loads only')
        self.assertFalse(report['packageable'])
        self.assertEqual(report['runtime_profile']['status'], 'unqualified')
        self.assertEqual(report['redistribution_evidence']['status'], 'unreviewed')
        self.assertEqual(before, sorted(str(p.relative_to(self.root)) for p in self.root.rglob('*')))

    def test_rpath_is_inherited_in_loader_chain(self):
        library = self.file(self.prefix / 'lib/first.dylib')
        dependency = self.file(self.prefix / 'lib/second.dylib')
        self.images[self.binary] = info([str(library)], [str(library.parent)])
        self.images[library] = info(['@rpath/second.dylib'])
        report = self.audit()
        self.assertFalse(report['static_closure']['issues'])
        self.assertIn(str(dependency), report['static_closure']['images'])

    def test_plugin_inherits_both_main_and_scanner_rpath_contexts(self):
        plugin = self.file(self.prefix / 'lib/gstreamer-1.0/plugin.dylib')
        dependency = self.file(self.prefix / 'lib/dependency.dylib')
        self.images[self.binary] = info(rpaths=[str(dependency.parent)])
        self.images[self.scanner] = info(rpaths=['@executable_path/../../lib'])
        self.images[plugin] = info(['@rpath/dependency.dylib'])
        report = self.audit()
        self.assertFalse(report['static_closure']['issues'])
        contexts = {e['executable_context'] for e in report['static_closure']['edges'] if e['from'] == str(plugin)}
        self.assertEqual(contexts, {str(self.binary), str(self.scanner)})

    def test_plugin_scanner_and_main_may_resolve_differently(self):
        plugin = self.file(self.prefix / 'lib/gstreamer-1.0/plugin.dylib')
        first = self.file(self.prefix / 'first/dependency.dylib')
        second = self.file(self.prefix / 'second/dependency.dylib')
        self.images[self.binary] = info(rpaths=[str(first.parent)])
        self.images[self.scanner] = info(rpaths=[str(second.parent)])
        self.images[plugin] = info(['@rpath/dependency.dylib'])
        report = self.audit()
        targets = {e['resolved'] for e in report['static_closure']['edges'] if e['from'] == str(plugin)}
        self.assertEqual(targets, {str(first), str(second)})
        self.assertIn('collision', str(report['static_closure']['issues']))

    def test_executable_and_loader_context(self):
        library = self.file(self.prefix / 'lib/first.dylib')
        dependency = self.file(self.prefix / 'lib/second.dylib')
        self.images[self.binary] = info([str(library)])
        self.images[library] = info(['@loader_path/second.dylib'])
        report = self.audit()
        self.assertIn(str(dependency), report['static_closure']['images'])
        self.assertEqual(runtime.expand('@executable_path/child', library, self.binary), self.binary.parent / 'child')

    def test_rpath_ambiguity_and_missing_are_not_guessed(self):
        first = self.file(self.prefix / 'one/same.dylib')
        second = self.file(self.prefix / 'two/same.dylib')
        self.images[self.binary] = info(['@rpath/same.dylib'], [str(first.parent), str(second.parent)])
        report = self.audit()
        self.assertIn('2 existing candidates', str(report['static_closure']['issues']))
        self.images[self.binary] = info(['@rpath/absent.dylib'])
        self.assertIn('0 existing candidates', str(self.audit()['static_closure']['issues']))

    def test_symlink_escape_is_reported_without_inspection(self):
        outside = self.file(self.root / 'outside.dylib')
        plugin = self.prefix / 'lib/gstreamer-1.0/escape.dylib'
        plugin.symlink_to(outside)
        report = self.audit()
        self.assertIn('escapes explicit roots', str(report['static_closure']['issues']))
        self.assertNotIn(str(outside), report['static_closure']['images'])

    def test_allowed_symlink_preserves_requested_path(self):
        target = self.file(self.prefix / 'lib/actual.dylib')
        plugin = self.prefix / 'lib/gstreamer-1.0/alias.dylib'
        plugin.symlink_to(target)
        self.assertIn(str(plugin), self.audit()['static_closure']['images'][str(target)]['requested_paths'])

    def test_broken_plugin_is_an_inventory_issue_not_required_functionality(self):
        plugin = self.prefix / 'lib/gstreamer-1.0/missing.dylib'
        plugin.symlink_to(self.prefix / 'absent.dylib')
        with patch.object(runtime, 'image_info', side_effect=lambda path, *_: info() if path.is_file() else
                          (_ for _ in ()).throw(FileNotFoundError(str(path)))):
            report = runtime.audit_runtime(self.config)
        self.assertTrue(report['static_closure']['issues'])
        self.assertIn('not a required-functionality profile', report['runtime_profile']['scope'])

    def test_flat_casefold_collision_and_deployment_incompatibility(self):
        first = self.file(self.prefix / 'one/same.dylib')
        second = self.file(self.prefix / 'two/SAME.dylib')
        self.images[self.binary] = info([str(first), str(second)])
        self.images[first] = info(minimum='27.0')
        report = self.audit()
        self.assertIn('collision', str(report['static_closure']['issues']))
        self.assertEqual(report['deployment_compatibility']['status'], 'incompatible')
        self.assertEqual(report['deployment_compatibility']['requested_minimum'], '13.0')

    def test_traversal_bound(self):
        report = self.audit(runtime.AuditConfig(self.binary, self.prefix, self.config.roots, max_images=1))
        self.assertIn('bound', str(report['static_closure']['issues']))
        self.assertLessEqual(len(report['static_closure']['images']), 1)

    def test_time_bound(self):
        with patch.object(runtime.time, 'monotonic', side_effect=[0, 601]):
            report = self.audit()
        self.assertIn('time/context bound', str(report['static_closure']['issues']))

    def test_directory_discovery_is_bounded_before_sorting(self):
        for index in range(3):
            self.file(self.prefix / f'lib/gstreamer-1.0/{index}.dylib')
        report = self.audit(runtime.AuditConfig(self.binary, self.prefix, self.config.roots, max_images=2))
        self.assertIn('directory entry bound', str(report['static_closure']['issues']))
        self.assertEqual(report['runtime_profile']['plugin_candidates'], [])

    def test_system_path_normalization_cannot_escape_to_non_system(self):
        with self.assertRaisesRegex(ValueError, 'escapes'):
            runtime.resolve_load('/usr/lib/../../opt/evil.dylib', self.binary, self.binary, (), self.config.roots)

    def test_formula_hints_do_not_become_license_approval(self):
        (self.prefix / 'INSTALL_RECEIPT.json').write_text('{}')
        formula = self.prefix / '.brew/runtime.rb'
        formula.parent.mkdir()
        formula.write_text('  license "LGPL-2.1-or-later"\n\n  depends_on "libsoup" => :no_linkage # dlopen\n')
        result = runtime.formula_evidence(self.scanner, self.config.roots)
        self.assertEqual(result['top_level_notice_candidates'], [])
        self.assertIn('libsoup', result['dynamic_dependency_hints'][0])
        self.assertEqual(result['component_license_and_source_provision'], 'not reviewed')

    def test_metadata_escapes_are_rejected(self):
        for kind in ['receipt', 'formula', 'formula-directory', 'notice']:
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as temp:
                root = Path(temp).resolve()
                package = root / 'allowed/pkg/1'
                package.mkdir(parents=True)
                (package / 'INSTALL_RECEIPT.json').write_text('{}')
                (package / '.brew').mkdir()
                outside = root / 'outside'
                outside.mkdir()
                (outside / 'payload').write_text('private outside roots')
                if kind == 'receipt':
                    (package / 'INSTALL_RECEIPT.json').unlink()
                    (package / 'INSTALL_RECEIPT.json').symlink_to(outside / 'payload')
                elif kind == 'formula':
                    (package / '.brew/pkg.rb').symlink_to(outside / 'payload')
                elif kind == 'formula-directory':
                    (package / '.brew').rmdir()
                    (package / '.brew').symlink_to(outside)
                else:
                    (package / 'LICENSE').symlink_to(outside / 'payload')
                with self.assertRaisesRegex(ValueError, 'escapes explicit roots'):
                    runtime.formula_evidence(package / 'lib/item.dylib', (root / 'allowed',))

    def test_oversized_metadata_is_rejected_without_reading(self):
        receipt = self.prefix / 'INSTALL_RECEIPT.json'
        with receipt.open('wb') as stream:
            stream.truncate(2 * 1024**2 + 1)
        with self.assertRaisesRegex(ValueError, 'byte audit limit'):
            runtime.formula_evidence(self.scanner, self.config.roots)


class MachOTests(unittest.TestCase):
    def test_fifo_rejected_without_opening_or_invoking_tools(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / 'pipe'
            os.mkfifo(path)
            with patch.object(Path, 'open') as opened, patch.object(runtime, 'run') as run:
                with self.assertRaisesRegex(ValueError, 'regular file'):
                    runtime.image_info(path, 'arm64', 1)
            opened.assert_not_called()
            run.assert_not_called()

    def test_non_macho_never_invokes_tools(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / 'fake.dylib'
            path.write_text('not code')
            with patch.object(runtime, 'run') as run, self.assertRaisesRegex(ValueError, 'Mach-O'):
                runtime.image_info(path, 'arm64', 1)
            run.assert_not_called()

    def test_each_architecture_and_load_kind_is_inspected(self):
        loads = ('Load command 0\n cmd LC_LOAD_WEAK_DYLIB\n name /usr/lib/libSystem.B.dylib (offset 24)\n'
                 'Load command 1\n cmd LC_ID_DYLIB\n name @rpath/fixture.dylib (offset 24)\n'
                 'Load command 2\n cmd LC_BUILD_VERSION\n platform 1\n minos 13.0\n'
                 'Load command 3\n cmd LC_RPATH\n path @loader_path (offset 12)\n')
        header = 'magic cputype cpusubtype caps filetype\nMH_MAGIC_64 ARM64 ALL 0x00 DYLIB\n'
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / 'fixture.dylib'
            path.write_bytes(bytes.fromhex('cffaedfe'))
            with patch.object(runtime, 'run', side_effect=['arm64 x86_64', loads, header, loads, header]) as run:
                report = runtime.image_info(path, 'universal', 1)
            self.assertEqual(set(report['slices']), {'arm64', 'x86_64'})
            self.assertEqual(report['slices']['arm64']['install_id'], '@rpath/fixture.dylib')
            self.assertEqual(report['slices']['arm64']['dependencies'][0]['kind'], 'LC_LOAD_WEAK_DYLIB')
            self.assertEqual(len(run.call_args_list), 5)


if __name__ == '__main__':
    unittest.main()
