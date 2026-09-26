#!/usr/bin/env python3
"""Behavioral tests for source identity and paired Wine loader contracts."""
import importlib.util
import hashlib
import json
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
    def test_spec_and_release(self):
        with self.subTest(phase="spec_correction_requires_both_exact_hashes"):
            with tempfile.TemporaryDirectory() as temporary:
                original, derived = [Path(temporary) / name for name in ('original', 'derived')]
                data = b'%files twain\n' + C.TWAIN_FILES + b'twaindsm.dll\nsane.ds\nsane.so\n'
                original.write_bytes(data)
                fixed = data.replace(C.TWAIN_FILES, C.TWAIN_FILES_FIXED)
                pin = {'spec_sha256': C.sha(original),
                       'build_spec_sha256': hashlib.sha256(fixed).hexdigest()}
                C.prepare_spec(original, derived, pin)
                self.assertEqual(derived.read_bytes(), fixed)
                derived.unlink()
                with self.assertRaisesRegex(ValueError, 'derived Wine spec'):
                    C.prepare_spec(original, derived, dict(pin, build_spec_sha256='0' * 64))
                self.assertFalse(derived.exists())
                original.write_bytes(data + b'unexpected change')
                with self.assertRaisesRegex(ValueError, 'input identity'):
                    C.prepare_spec(original, derived, pin)
        with self.subTest(phase="dist_macro_produces_distinct_release_and_tracks_epoch"):
            pin = json.loads((Path(__file__).resolve().parents[1] / 'dev/wine-source.json').read_text())
            identity = C.build_identity(pin)
            self.assertEqual(identity['release'], '3.fc44.tilo_traditional1')
            self.assertNotEqual(identity['release'], pin['release'])
            pin['source_date_epoch'] += 1
            self.assertEqual(C.build_identity(pin)['source_date_epoch'], pin['source_date_epoch'])
            pin['build_release'] = pin['release']
            with self.assertRaises(ValueError):
                C.build_identity(pin)
        with self.subTest(phase="source_must_match_bytes_size_and_regular_file"):
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
        with self.subTest(phase="moving_buildroot_and_foreign_target_rejected"):
            C.reference('registry.example/wine@sha256:' + 'a' * 64)
            for value in ('registry.example/wine:latest', 'wine@sha256:abc', 'wine@sha256:' + 'G' * 64):
                with self.assertRaises(ValueError):
                    C.reference(value)
            with self.assertRaises(ValueError):
                C.target('aarch64')

    def test_traditional_layout(self):
        with self.subTest(phase="supported_traditional_layout_both_targets"):
            for arch in ('i686', 'x86_64'):
                C.validate_core_layout(arch, layout(arch))
        with self.subTest(phase="missing_or_misdirected_cross_width_loader_rejected"):
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
        with self.subTest(phase="new_wow64_payload_rejected_even_with_added_links"):
            files = layout('x86_64')
            files['/usr/lib64/wine-wow64/wine/x86_64-unix/ntdll.so'] = ''
            with self.assertRaisesRegex(ValueError, 'new WoW64'):
                C.validate_core_layout('x86_64', files)
        with self.subTest(phase="native_loader_cannot_be_a_redirecting_symlink"):
            files = layout('i686')
            files['/usr/bin/wine32'] = '/tmp/substitute'
            with self.assertRaisesRegex(ValueError, 'regular native loader'):
                C.validate_core_layout('i686', files)
        with self.subTest(phase="elf_headers_must_match_requested_target"):
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

    def test_rpm_query_output(self):
        with self.subTest(phase="empty_core_output_is_not_an_artifact"):
            with tempfile.TemporaryDirectory() as temporary:
                directory = Path(temporary)
                (directory / 'rpms').mkdir()
                with self.assertRaisesRegex(ValueError, 'wine-core'):
                    C.output('i686', {}, 'repo@sha256:' + 'a' * 64, directory)
        with self.subTest(phase="query_retains_empty_link_field_on_last_file"):
            with patch.object(C.subprocess, 'check_output', return_value='/loader\t\n'):
                self.assertEqual(C.run(['rpm']), '/loader\t')


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


class MockContractTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.raw, self.bundle, self.out = [self.root / name for name in ('raw', 'bundle', 'out')]
        self.pin = json.loads((Path(__file__).resolve().parents[1] / 'dev/wine-source.json').read_text())
        self.ref = 'registry.example/resolution@sha256:' + 'a' * 64
        def write(root, name, value):
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(json.dumps(value) if isinstance(value, dict) else value)
        self.write = write
        upstream = b'%files twain\n' + C.TWAIN_FILES + b'twaindsm.dll\n'
        derived = upstream.replace(C.TWAIN_FILES, C.TWAIN_FILES_FIXED)
        self.pin['spec_sha256'] = hashlib.sha256(upstream).hexdigest()
        self.pin['build_spec_sha256'] = hashlib.sha256(derived).hexdigest()
        write(self.bundle, 'build-source/upstream.spec', upstream.decode())
        write(self.bundle, 'build-source/wine.spec', derived.decode())
        srpm_name = C.build_identity(self.pin)['source_rpm']
        write(self.bundle, 'build-source/' + srpm_name, 'derived source')
        write(self.bundle, 'build-source/derivation.json', {
            'schema': 1, 'correction': 'wine11-twain-32-i386-file-list',
            'upstream_srpm_sha256': self.pin['sha256'],
            'upstream_spec_sha256': self.pin['spec_sha256'],
            'build_spec_sha256': self.pin['build_spec_sha256'],
            'source_rpm': srpm_name,
            'source_rpm_sha256': C.sha(self.bundle / 'build-source' / srpm_name)})
        write(self.bundle, 'source/' + self.pin['filename'], 'source')
        write(self.bundle, 'source/fedora-44.asc', 'key')
        write(self.bundle, 'source/download.log', 'source acquisition evidence')
        write(self.bundle, 'resolve.log', 'resolution command log')
        write(self.bundle, 'mock-config/fedora.cfg', 'target config')
        write(self.bundle, 'repository/package.rpm', 'resolved dependency')
        write(self.bundle, 'resolution/buildroot_lock.json', {
            'version': '1.1.0', 'config': {'target_arch': 'x86_64', 'bootstrap_image_ready': True},
            'bootstrap': {'image_digest': 'sha256:' + 'b' * 64}, 'buildroot': {'rpms': [{'name': 'gcc'}]}})
        options = ['--target', 'x86_64', '--without', 'new_wow64', '--define',
                   'dist ' + self.pin['build_dist'], '--define', '_smp_mflags -j2']
        shared = {'schema': 1, 'mode': 'mock-hermetic', 'target': 'x86_64', 'source': self.pin,
                  'options': options, 'tools': {'runtime_image': 'registry.example/tools@sha256:' + 'c' * 64,
                      'mock_version': '5.7', 'host_rpms': ['mock'], 'mock_api_reference_commit': 'd' * 40,
                      'tools': {name: {'path': '/usr/bin/' + name, 'sha256': 'e' * 64}
                                for name in ('mock', 'mock-hermetic-repo', 'rpm', 'podman')}}}
        write(self.bundle, 'manifest.json', {**shared, 'files': C.tree_inventory(self.bundle)})
        write(self.raw, 'rpms-and-logs/wine-core.x86_64.rpm', 'binary')
        write(self.raw, 'rpms-and-logs/' + C.build_identity(self.pin)['source_rpm'], 'rebuilt source')
        write(self.raw, 'rpms-and-logs/build.log', 'complete build log')
        write(self.raw, 'hermetic-build.log', 'offline command log')
        for name in ('installed-rpms.log', 'buildrequires.log', 'gcc-version.log', 'g++-version.log', 'rpmbuild-version.log'):
            write(self.raw, name, 'tool evidence')
        write(self.raw, 'compiler-probes.json', {name: {'elf_class': 2, 'elf_machine': 62, 'rpm_optflags': ['-O2']}
                                                for name in ('gcc', 'g++')})
        write(self.raw, 'producer.json', {**shared, 'build_identity': C.build_identity(self.pin),
            'network_interfaces': ['lo'], 'execution_validated': False,
            'resolution_manifest_sha256': C.sha(self.bundle / 'manifest.json'),
            'lockfile_sha256': C.sha(self.bundle / 'resolution/buildroot_lock.json'),
            'files': C.tree_inventory(self.raw)})
        def query(argv):
            if 'FILENAMES' in argv[3]:
                return '\n'.join(name + '\t' + value for name, value in layout('x86_64').items())
            return '\t'.join(['wine-core', self.pin['version'], self.pin['build_release'],
                              'x86_64', C.build_identity(self.pin)['source_rpm'], 'LGPL'])
        for mocked in (patch.object(C, 'source', return_value='signature verified'),
                       patch.object(C, 'verify_srpm_spec'),
                       patch.object(C, 'run', side_effect=query),
                       patch.object(C, 'rpm_loader_identity', return_value={'elf_class': 2, 'elf_machine': 62})):
            mocked.start()
            self.addCleanup(mocked.stop)

    def import_output(self):
        C.import_mock('x86_64', self.pin, self.raw, self.bundle, self.out, self.ref)

    def test_prepared_source_binding(self):
        with self.subTest(phase="prepared_source_rejects_changed_spec_and_source_bytes"):
            prepared = self.bundle / 'build-source'
            C.verify_prepared_source(prepared, self.pin)
            for name in ('upstream.spec', 'wine.spec', C.build_identity(self.pin)['source_rpm']):
                path = prepared / name
                original = path.read_bytes()
                path.write_bytes(original + b'changed')
                with self.subTest(name=name), self.assertRaisesRegex(ValueError, 'input identity'):
                    C.verify_prepared_source(prepared, self.pin)
                path.write_bytes(original)
        restart_fixture(self)
        with self.subTest(phase="prepared_source_rejects_false_upstream_binding"):
            path = self.bundle / 'build-source/derivation.json'
            record = json.loads(path.read_text())
            record['upstream_srpm_sha256'] = '0' * 64
            path.write_text(json.dumps(record))
            with self.assertRaisesRegex(ValueError, 'derivation identity'):
                C.verify_prepared_source(path.parent, self.pin)
        restart_fixture(self)
        with self.subTest(phase="option_or_target_change_rejected"):
            for field, value in (('target', 'i686'), ('options', ['--without', 'alsa'])):
                path = self.raw / 'producer.json'
                original = path.read_text()
                record = json.loads(original)
                record[field] = value
                self.write(self.raw, 'producer.json', record)
                with self.subTest(field=field), self.assertRaises(ValueError):
                    self.import_output()
                path.write_text(original)
        restart_fixture(self)
        with self.subTest(phase="legacy_output_and_mode_confusion"):
            self.import_output()
            old = C.output('x86_64', self.pin, self.ref, self.out, write=False)
            self.assertEqual(old['buildroot_ref'], self.ref)
            self.assertNotIn('producer', old)
            for mode, buildroot in (('unknown', None), ('mock-hermetic', self.ref)):
                with self.assertRaises(ValueError):
                    C.output('x86_64', self.pin, buildroot, self.out, write=False, mode=mode)

    def test_import_tamper_detection(self):
        with self.subTest(phase="import_preserves_logs_and_sources_without_fake_buildroot"):
            self.import_output()
            stored = json.loads((self.out / 'provenance/contract.json').read_text())
            self.assertNotIn('buildroot_ref', stored)
            self.assertEqual(stored['producer']['resolution_ref'], self.ref)
            self.assertEqual((self.out / 'provenance/mock/rpms-and-logs/build.log').read_text(), 'complete build log')
            self.assertTrue((self.out / 'provenance' / self.pin['filename']).exists())
            self.assertEqual((self.out / 'provenance/resolution/source/download.log').read_text(), 'source acquisition evidence')
            self.assertEqual(stored, C.output('x86_64', self.pin, None, self.out, write=False, mode='mock-hermetic'))
        restart_fixture(self)
        with self.subTest(phase="raw_payload_tamper_prevents_import"):
            (self.raw / 'rpms-and-logs/wine-core.x86_64.rpm').write_text('tampered')
            with self.assertRaisesRegex(ValueError, 'raw Mock output changed'):
                self.import_output()
            self.assertFalse(self.out.exists())
        restart_fixture(self)
        with self.subTest(phase="dependency_bundle_tamper_prevents_import"):
            (self.bundle / 'repository/package.rpm').write_text('tampered')
            with self.assertRaisesRegex(ValueError, 'resolution bundle changed'):
                self.import_output()
        restart_fixture(self)
        with self.subTest(phase="normalized_log_lock_and_config_tampering_detected"):
            self.import_output()
            for relative in ('mock/rpms-and-logs/build.log', 'buildroot_lock.json', 'mock-config/fedora.cfg',
                             'resolution/source/download.log'):
                path = self.out / 'provenance' / relative
                original = path.read_bytes()
                path.write_bytes(original + b' ')
                with self.subTest(path=relative), self.assertRaises(ValueError):
                    C.output('x86_64', self.pin, None, self.out, write=False, mode='mock-hermetic')
                path.write_bytes(original)
        restart_fixture(self)
        with self.subTest(phase="extra_normalized_rpm_rejected"):
            self.import_output()
            (self.out / 'rpms/extra.rpm').write_text('extra')
            with self.assertRaisesRegex(ValueError, 'RPM inventory changed'):
                C.output('x86_64', self.pin, None, self.out, write=False, mode='mock-hermetic')


if __name__ == '__main__':
    unittest.main()
