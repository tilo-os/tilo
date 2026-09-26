#!/usr/bin/env python3
"""Boundary tests for the Mock input handoff; no Mock/build simulation claims."""
import importlib.util
import io
import json
import platform
from pathlib import Path
import shutil
import sys
import subprocess
import tempfile
import unittest
from unittest.mock import patch

DEV = Path(__file__).resolve().parents[1] / 'dev'
sys.path.insert(0, str(DEV))
SPEC = importlib.util.spec_from_file_location('wine_mock_producer', DEV / 'wine-mock-producer.py')
producer = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(producer)
PIN = json.loads((DEV / 'wine-source.json').read_text())
IMAGE = 'example.invalid/mock@sha256:' + 'a' * 64


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


class Boundaries(unittest.TestCase):
    def test_i686_probe_and_diagnostics(self):
        with self.subTest(phase="native_i686_futex_probe_accepts_abi_and_rejects_missing_syscall"):
            if not (platform.machine() == 'x86_64' and shutil.which('gcc')):
                self.skipTest('requires native x86-64 gcc')
            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                producer.verify_i686_execution(root)
                original_run = producer.run
                def unavailable_syscall(argv, log):
                    if argv[0] == 'gcc':
                        source = root / 'i686-execution.S'
                        source.write_text(source.read_text().replace('mov $240, %eax', 'mov $-1, %eax'))
                    return original_run(argv, log)
                with patch.object(producer, 'run', side_effect=unavailable_syscall):
                    with self.assertRaisesRegex(ValueError, 'legacy futex ABI'):
                        producer.verify_i686_execution(root)
        restart_fixture(self)
        with self.subTest(phase="streamed_diagnostics_keep_exact_bytes_and_failure"):
            with tempfile.TemporaryDirectory() as temporary:
                log = Path(temporary) / 'command.log'
                stream = io.TextIOWrapper(io.BytesIO())
                command = [sys.executable, '-c',
                           "import os; os.write(1, b'progress\\n'); os.write(2, b'failure\\xff\\n'); raise SystemExit(7)"]
                with patch.object(producer.sys, 'stdout', stream):
                    with self.assertRaises(subprocess.CalledProcessError) as failure:
                        producer.run(command, log)
                self.assertEqual(failure.exception.returncode, 7)
                self.assertEqual(stream.buffer.getvalue(), b'progress\nfailure\xff\n')
                self.assertEqual(log.read_bytes().split(b'\n', 1)[1], stream.buffer.getvalue())
        restart_fixture(self)
        with self.subTest(phase="i686_execution_failure_keeps_kernel_diagnostic"):
            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                header = bytearray(20)
                header[:6] = b'\x7fELF\x01\x01'
                header[18:20] = (3).to_bytes(2, 'little')

                def invoke(argv, log):
                    if argv[0] == 'gcc':
                        Path(argv[-1]).write_bytes(header)
                    else:
                        raise OSError(8, 'Exec format error')

                with patch.object(producer, 'run', side_effect=invoke):
                    with self.assertRaisesRegex(ValueError, 'IA32 execution is required'):
                        producer.verify_i686_execution(root)
                self.assertIn('Exec format error', (root / 'i686-execution.log').read_text())
        restart_fixture(self)
        with self.subTest(phase="failure_keeps_log"):
            log = self.root / 'failed.log'
            with self.assertRaises(producer.subprocess.CalledProcessError):
                producer.run([sys.executable, '-c', 'print("failure evidence"); exit(3)'], log)
            self.assertIn('failure evidence', log.read_text())

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / 'resolution').mkdir()
        self.lock = {'version': '1.1.0', 'config': {
            'target_arch': 'i686', 'bootstrap_image_ready': True},
            'bootstrap': {'image_digest': 'sha256:' + 'b' * 64},
            'buildroot': {'rpms': [{'name': name, 'arch': 'i686'}
                                  for name in ('gcc', 'gcc-c++', 'rpm-build')]}}
        producer.write_json(self.root / 'resolution/buildroot_lock.json', self.lock)
        (self.root / 'dependency.rpm').write_bytes(b'retained dependency')
        self.tools = {'runtime_image': IMAGE, 'host_rpms': ['b', 'a'], 'tools': {}}
        self.record = {'schema': 1, 'mode': 'mock-hermetic', 'target': 'i686',
            'source': PIN, 'options': producer.flags('i686', PIN, 2),
            'tools': self.tools, 'files': producer.inventory(self.root)}
        self.save()

    def save(self):
        producer.write_json(self.root / 'manifest.json', self.record)

    def verify(self, **kwargs):
        with patch.object(producer, 'tool_record', return_value=dict(self.tools)):
            return producer.verify_bundle(self.root, IMAGE, kwargs.get('target', 'i686'),
                                          PIN, kwargs.get('jobs', 2))

    def test_bundle_inputs(self):
        with self.subTest(phase="exact_bundle_accepted"):
            self.assertEqual(self.verify()['target'], 'i686')
        restart_fixture(self)
        with self.subTest(phase="tool_comparison_preserves_original_evidence_order"):
            original_manifest = (self.root / 'manifest.json').read_bytes()
            current_tools = dict(self.tools, host_rpms=['a', 'b'])
            with patch.object(producer, 'tool_record', return_value=current_tools):
                verified = producer.verify_bundle(self.root, IMAGE, 'i686', PIN, 2)
            self.assertEqual(verified['tools']['host_rpms'], ['b', 'a'])
            self.assertEqual(verified['tools'], self.record['tools'])
            self.assertEqual(current_tools['host_rpms'], ['a', 'b'])
            self.assertEqual((self.root / 'manifest.json').read_bytes(), original_manifest)
        restart_fixture(self)
        with self.subTest(phase="modified_rpm_rejected"):
            (self.root / 'dependency.rpm').write_bytes(b'changed')
            with self.assertRaisesRegex(ValueError, 'bundle changed'):
                self.verify()
        restart_fixture(self)
        with self.subTest(phase="added_or_missing_input_rejected"):
            (self.root / 'extra').write_text('unexpected')
            with self.assertRaisesRegex(ValueError, 'bundle changed'):
                self.verify()
            (self.root / 'extra').unlink()
            (self.root / 'dependency.rpm').unlink()
            with self.assertRaisesRegex(ValueError, 'bundle changed'):
                self.verify()
        restart_fixture(self)
        with self.subTest(phase="cross_target_and_options_rejected"):
            for values in ({'target': 'x86_64'}, {'jobs': 4}):
                with self.subTest(values=values), self.assertRaisesRegex(ValueError, 'options differ'):
                    self.verify(**values)
        restart_fixture(self)
        with self.subTest(phase="changed_tool_image_rejected"):
            tools = dict(self.tools, runtime_image='different@sha256:' + 'c' * 64)
            with patch.object(producer, 'tool_record', return_value=tools):
                with self.assertRaisesRegex(ValueError, 'tool environment differs'):
                    producer.verify_bundle(self.root, IMAGE, 'i686', PIN, 2)

    def test_lock_provenance(self):
        with self.subTest(phase="incomplete_lock_rejected"):
            for field in ('bootstrap', 'buildroot', 'config'):
                lock = dict(self.lock)
                del lock[field]
                with self.subTest(field=field), self.assertRaises(ValueError):
                    producer.validate_lock(lock, 'i686')
        restart_fixture(self)
        with self.subTest(phase="wrong_lock_architecture_rejected"):
            with self.assertRaises(ValueError):
                producer.validate_lock(self.lock, 'x86_64')
        restart_fixture(self)
        with self.subTest(phase="lock_requires_native_provenance_compilers"):
            for name in ('gcc', 'gcc-c++', 'rpm-build'):
                for replacement in (None, 'x86_64', 'noarch'):
                    lock = json.loads(json.dumps(self.lock))
                    lock['buildroot']['rpms'] = [package for package in lock['buildroot']['rpms']
                                                if package['name'] != name]
                    if replacement:
                        lock['buildroot']['rpms'].append({'name': name, 'arch': replacement})
                    with self.subTest(name=name, replacement=replacement):
                        with self.assertRaisesRegex(ValueError, 'native provenance tools'):
                            producer.validate_lock(lock, 'i686')

    def test_config_and_mode(self):
        with self.subTest(phase="retained_config_adds_tools_without_changing_vendor_config"):
            source = self.root / 'vendor-config'
            source.mkdir()
            selected = source / 'fedora-44-i386.cfg'
            original = "config_opts['more_buildreqs']['wine'] = ['existing-dependency']\n"
            selected.write_text(original)
            (source / 'site-defaults.cfg').write_text('# preserved defaults\n')
            destination = self.root / 'retained-config'
            producer.prepare_mock_config(source, destination, 'fedora-44-i386')
            options = {'more_buildreqs': {'other-package': ['unrelated']}}
            exec((destination / selected.name).read_text(), {'config_opts': options})
            self.assertEqual(options['more_buildreqs']['wine'],
                             ['existing-dependency', 'gcc', 'gcc-c++', 'rpm-build'])
            self.assertEqual(options['more_buildreqs']['other-package'], ['unrelated'])
            self.assertEqual(selected.read_text(), original)
            self.assertEqual((destination / 'site-defaults.cfg').read_bytes(),
                             (source / 'site-defaults.cfg').read_bytes())
        restart_fixture(self)
        with self.subTest(phase="config_selection_cannot_escape_retained_directory"):
            for name in ('../fedora', '/tmp/fedora', r'..\fedora'):
                with self.subTest(name=name), self.assertRaisesRegex(ValueError, 'configuration name'):
                    producer.prepare_mock_config(self.root, self.root / 'unused', name)
        restart_fixture(self)
        with self.subTest(phase="network_interface_rejected"):
            interfaces = self.root / 'interfaces'
            interfaces.mkdir()
            (interfaces / 'lo').mkdir()
            producer.offline_network(interfaces)
            (interfaces / 'eth0').mkdir()
            with self.assertRaisesRegex(ValueError, 'network namespace'):
                producer.offline_network(interfaces)
        restart_fixture(self)
        with self.subTest(phase="only_traditional_mode_flags"):
            options = producer.flags('i686', PIN, 2)
            self.assertEqual(options[options.index('--without') + 1], 'new_wow64')
            self.assertIn('dist ' + PIN['build_dist'], options)
            self.assertNotIn('--nodeps', options)
            with self.assertRaises(ValueError):
                producer.flags('i686', PIN, 0)
        restart_fixture(self)
        with self.subTest(phase="hermetic_rebuild_uses_supported_exclusive_mode"):
            command = producer.build_command(self.root, self.root / 'fresh-root',
                self.root / 'results', producer.flags('i686', PIN, 2), PIN['filename'])
            self.assertEqual(command[1:4], ['--hermetic-build',
                str(self.root / 'resolution/buildroot_lock.json'), str(self.root / 'repository')])
            for forbidden in ('-r', '--init', '--calculate-build-dependencies', '--no-clean', '--short-circuit'):
                self.assertNotIn(forbidden, command)
            self.assertEqual(command[-1], str(self.root / 'build-source' / PIN['filename']))


if __name__ == '__main__':
    unittest.main()
