#!/usr/bin/env python3
"""Boundary tests for the Mock input handoff; no Mock/build simulation claims."""
import importlib.util
import json
from pathlib import Path
import sys
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


class Boundaries(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / 'resolution').mkdir()
        self.lock = {'version': '1.1.0', 'config': {
            'target_arch': 'i686', 'bootstrap_image_ready': True},
            'bootstrap': {'image_digest': 'sha256:' + 'b' * 64},
            'buildroot': {'rpms': [{'name': 'gcc', 'arch': 'i686'}]}}
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

    def test_exact_bundle_accepted(self):
        self.assertEqual(self.verify()['target'], 'i686')

    def test_modified_rpm_rejected(self):
        (self.root / 'dependency.rpm').write_bytes(b'changed')
        with self.assertRaisesRegex(ValueError, 'bundle changed'):
            self.verify()

    def test_added_or_missing_input_rejected(self):
        (self.root / 'extra').write_text('unexpected')
        with self.assertRaisesRegex(ValueError, 'bundle changed'):
            self.verify()
        (self.root / 'extra').unlink()
        (self.root / 'dependency.rpm').unlink()
        with self.assertRaisesRegex(ValueError, 'bundle changed'):
            self.verify()

    def test_cross_target_and_options_rejected(self):
        for values in ({'target': 'x86_64'}, {'jobs': 4}):
            with self.subTest(values=values), self.assertRaisesRegex(ValueError, 'options differ'):
                self.verify(**values)

    def test_changed_tool_image_rejected(self):
        tools = dict(self.tools, runtime_image='different@sha256:' + 'c' * 64)
        with patch.object(producer, 'tool_record', return_value=tools):
            with self.assertRaisesRegex(ValueError, 'tool environment differs'):
                producer.verify_bundle(self.root, IMAGE, 'i686', PIN, 2)

    def test_incomplete_lock_rejected(self):
        for field in ('bootstrap', 'buildroot', 'config'):
            lock = dict(self.lock)
            del lock[field]
            with self.subTest(field=field), self.assertRaises(ValueError):
                producer.validate_lock(lock, 'i686')

    def test_wrong_lock_architecture_rejected(self):
        with self.assertRaises(ValueError):
            producer.validate_lock(self.lock, 'x86_64')

    def test_network_interface_rejected(self):
        interfaces = self.root / 'interfaces'
        interfaces.mkdir()
        (interfaces / 'lo').mkdir()
        producer.offline_network(interfaces)
        (interfaces / 'eth0').mkdir()
        with self.assertRaisesRegex(ValueError, 'network namespace'):
            producer.offline_network(interfaces)

    def test_only_traditional_mode_flags(self):
        options = producer.flags('i686', PIN, 2)
        self.assertEqual(options[options.index('--without') + 1], 'new_wow64')
        self.assertIn('dist ' + PIN['build_dist'], options)
        self.assertNotIn('--nodeps', options)
        with self.assertRaises(ValueError):
            producer.flags('i686', PIN, 0)

    def test_failure_keeps_log(self):
        log = self.root / 'failed.log'
        with self.assertRaises(producer.subprocess.CalledProcessError):
            producer.run([sys.executable, '-c', 'print("failure evidence"); exit(3)'], log)
        self.assertIn('failure evidence', log.read_text())

    def test_hermetic_rebuild_uses_supported_exclusive_mode(self):
        command = producer.build_command(self.root, self.root / 'fresh-root',
            self.root / 'results', producer.flags('i686', PIN, 2), PIN['filename'])
        self.assertEqual(command[1:4], ['--hermetic-build',
            str(self.root / 'resolution/buildroot_lock.json'), str(self.root / 'repository')])
        for forbidden in ('-r', '--init', '--calculate-build-dependencies', '--no-clean', '--short-circuit'):
            self.assertNotIn(forbidden, command)
        self.assertEqual(command[-1], str(self.root / 'source' / PIN['filename']))


if __name__ == '__main__':
    unittest.main()
