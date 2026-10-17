#!/usr/bin/env python3
"""Mutation tests of the production installed contracts (not native qualification)."""
import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

SCRIPTS = Path(__file__).resolve().parents[1] / 'scripts'
spec = importlib.util.spec_from_file_location('consumer', SCRIPTS / 'installed-data-consumer.py')
c = importlib.util.module_from_spec(spec)
spec.loader.exec_module(c)


class Contracts(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.store = c.Store(self.root, os.getuid())
        paths = [c.HELPER, '/usr/libexec/tilo-firstboot-user',
                 '/usr/libexec/tilo-firstboot-home-seeds', '/usr/libexec/tilo-firstboot-files',
                 '/usr/libexec/tilo-verify-home-consumer-capability',
                 '/usr/libexec/tilo-verify-applications-consumer-capability',
                 '/usr/libexec/tilo-installed-data-consumer.py', '/usr/bin/tar', *c.TOOLS]
        for name in paths:
            path = self.store.path(name)
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(name.encode())
            path.chmod(0o755)
        self.store.path('/usr/share/tilo').mkdir(parents=True)
        self.home = c.home_facts(self.store)
        self.facts = c.apps_artifacts(self.store)
        self.native = {'schema': 1, 'subset_protocol': 2, 'source_protocol': 6,
                       'prepared_helper_sha256': self.facts['prepared_helper_sha256'],
                       'tools': self.facts['tools'],
                       **{k: 'a' * 64 for k in ('source_read_claim_sha256', 'source_completion_receipt_sha256',
                                               'setup_ciphertext_sha256', 'applications_ciphertext_sha256')},
                       'receiving': {'schema': 1, 'subset_protocol': 2, 'deployments': 2, 'install_commands': 2,
                                     'commits': {'app/org.tilo.Fixture/x86_64/stable': 'b' * 64,
                                                 'runtime/org.tilo.FixtureRuntime/x86_64/stable': 'c' * 64},
                                     'selected_data_sha256': 'd' * 64}}
        self.cap = {'schema': 1, 'subset_protocol': 2,
                    'prepared_helper_sha256': self.facts['prepared_helper_sha256'],
                    'qualification_script_sha256': self.facts['qualification_script_sha256'],
                    'native_qualification_sha256': hashlib.sha256(c.canonical(self.native)).hexdigest()}
        # Explicit mocked unit data; these files never constitute native evidence.
        self.original = {'schema': 1, 'mocked_unit_test_only': True,
                         'original_source_protocol': 6,
                         'prepared_helper_sha256': self.facts['prepared_helper_sha256'],
                         'capture_pins': {'worker_protocol': 6}}
        self.save(c.HOME, self.home)
        self.save(c.APPS, self.cap)
        self.save(c.NATIVE, self.native, newline=False)
        self.save(c.ORIGINAL, self.original)

    def save(self, name, value, newline=True):
        self.store.path('/usr/share/tilo/' + name).write_bytes(c.canonical(value) + (b'\n' if newline else b''))

    def runner(self, command):
        mode = command[-1]
        if mode == '--home-consumer-capability':
            return c.canonical(self.home)
        if mode == '--capabilities':
            self.assertEqual(command, [str(self.store.path('/usr/libexec/tilo-firstboot-home-seeds')),
                                       '--capabilities'])
            return b'{"schema":1,"seed_protocol":1}'
        if mode == '--reattest-original-source-consumer-capability':
            return c.canonical(self.original)
        if mode == '--applications-consumer-capability':
            return c.canonical({'capability': self.cap, 'native_qualification': self.native,
                                'original_source': self.original})
        self.fail('unexpected production command: ' + repr(command))

    def test_closed_home_and_applications_checks_are_read_only(self):
        files = list(self.store.path('/usr/share/tilo').iterdir())
        stamps = {p: (p.read_bytes(), p.stat().st_mtime_ns) for p in files}
        self.assertEqual(c.home(self.store, check=True, runner=self.runner), self.home)
        self.assertEqual(c.applications(self.store, check=True, runner=self.runner), self.cap)
        self.assertEqual(stamps, {p: (p.read_bytes(), p.stat().st_mtime_ns) for p in files})

    def test_receipt_mutations(self):
        for name, proof, function, key in [(c.HOME, self.home, c.home, 'archive_protocol'),
                                           (c.APPS, self.cap, c.applications, 'subset_protocol')]:
            for mutate in (lambda p: p.update(unknown=True), lambda p: p.pop('schema'),
                           lambda p: p.update(schema=True), lambda p: p.update({key: 1}),
                           lambda p: p.update(prepared_helper_sha256='A' * 64)):
                value = copy.deepcopy(proof)
                mutate(value)
                self.save(name, value)
                with self.assertRaises(ValueError):
                    function(self.store, check=True, runner=self.runner)
            self.save(name, proof)

    def test_duplicate_and_oversized_proofs(self):
        for payload in (b'{"schema":1,"schema":1}', b' ' * 4097, b'[]'):
            self.store.path('/usr/share/tilo/' + c.HOME).write_bytes(payload)
            with self.assertRaises(ValueError):
                c.home(self.store, check=True, runner=self.runner)

    def test_every_actual_artifact_change_refuses(self):
        for name in [c.HELPER, '/usr/bin/tar', '/usr/libexec/tilo-firstboot-user',
                     '/usr/libexec/tilo-firstboot-home-seeds']:
            path = self.store.path(name)
            data = path.read_bytes()
            path.write_bytes(data + b'changed')
            with self.assertRaises(ValueError):
                c.home(self.store, check=True, runner=self.runner)
            path.write_bytes(data)
        for name in [c.HELPER, '/usr/libexec/tilo-verify-applications-consumer-capability', *c.TOOLS]:
            path = self.store.path(name)
            data = path.read_bytes()
            path.write_bytes(data + b'changed')
            with self.assertRaises(ValueError):
                c.applications(self.store, check=True, runner=self.runner)
            path.write_bytes(data)

    def test_compiled_firstboot_helper_substitution_during_probe_refuses(self):
        def changed(command):
            result = self.runner(command)
            if command[-1] == '--home-consumer-capability':
                self.store.path('/usr/libexec/tilo-firstboot-files').write_bytes(b'replaced ELF')
            return result
        with self.assertRaisesRegex(ValueError, 'Home artifacts changed during probe'):
            c.home(self.store, check=True, runner=changed)

    def test_namespace_redirect_and_mutable_receipt_refuse(self):
        path = self.store.path('/usr/share/tilo/' + c.APPS)
        path.chmod(0o666)
        with self.assertRaises(ValueError):
            c.applications(self.store, check=True, runner=self.runner)
        path.chmod(0o644)
        actual = path.with_suffix('.actual')
        path.rename(actual)
        path.symlink_to(actual.name)
        with self.assertRaises(ValueError):
            c.applications(self.store, check=True, runner=self.runner)

    def test_native_report_mutations_even_with_recomputed_digest(self):
        for mutate in (lambda p: p.update(source_protocol=5),
                       lambda p: p['receiving'].update(deployments=1),
                       lambda p: p['receiving'].update(install_commands=4),
                       lambda p: p['receiving'].update(commits=[]),
                       lambda p: p['receiving'].update(selected_data_sha256='g' * 64),
                       lambda p: p['tools'].update({'/usr/bin/flatpak': 'f' * 64})):
            native = copy.deepcopy(self.native)
            mutate(native)
            self.save(c.NATIVE, native, newline=False)
            cap = dict(self.cap, native_qualification_sha256=hashlib.sha256(c.canonical(native)).hexdigest())
            self.save(c.APPS, cap)
            with self.assertRaises(ValueError):
                c.applications(self.store, check=True, runner=self.runner)

    def test_signed_cohort_failure_and_during_check_substitution_refuse(self):
        def changed(command):
            if command[-1] == '--reattest-original-source-consumer-capability':
                self.store.path('/usr/bin/flatpak').write_bytes(b'replaced')
            return self.runner(command)
        with self.assertRaises(ValueError):
            c.applications(self.store, check=True, runner=changed)
        def failed(command):
            raise ValueError('canonical signature verification failed')
        with self.assertRaises(ValueError):
            c.applications(self.store, check=True, runner=failed)

    def test_producer_calls_real_factory_mode_and_publishes_completion_last(self):
        for name in (c.APPS, c.NATIVE, c.ORIGINAL):
            self.store.path('/usr/share/tilo/' + name).unlink()
        published = []
        def publish(name, data, limit):
            self.assertLessEqual(len(data), limit)
            published.append(name)
            self.store.path('/usr/share/tilo/' + name).write_bytes(data)
        with patch.object(c.os, 'geteuid', return_value=0), patch.object(self.store, 'publish', side_effect=publish):
            self.assertEqual(c.applications(self.store, runner=self.runner), self.cap)
        self.assertEqual(published, [c.ORIGINAL, c.NATIVE, c.APPS])

    def test_cli_rejects_path_or_receipt_input(self):
        for name in ('verify-home-consumer-capability.py', 'verify-applications-consumer-capability.py',
                     'data-consumer-catalog-facts.py'):
            completed = subprocess.run(['/usr/bin/python3', str(SCRIPTS / name), '--root', str(self.root)],
                                       capture_output=True, timeout=5)
            self.assertNotEqual(completed.returncode, 0)
            self.assertEqual(completed.stdout, b'')
            self.assertIn(b'accepts', completed.stderr)

    def test_catalog_publication_preserves_checked_facts_and_exact_image_binding(self):
        repo = SCRIPTS.parents[1]
        path = repo / 'apps/imager/packaging/media-provenance.py'
        spec = importlib.util.spec_from_file_location('media_provenance', path)
        media = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(media)
        qcow = self.root / 'unit-only.qcow2'
        qcow.write_bytes(b'unit test image, not qualification')
        snapshot = {'profile_json': None, 'definition': None,
                    'data_consumers': {'home_archive': self.home}}
        with patch.object(media.subprocess, 'check_output', side_effect=[
                'sha256:' + 'a' * 64 + ' amd64\n', json.dumps(snapshot)]):
            sidecar = media.create('mutable-tag', qcow)
        self.assertEqual(json.loads(sidecar.read_bytes())['data_consumers'], {'home_archive': self.home})
        result = subprocess.run(['/usr/bin/python3', str(path), 'consumer-facts', str(qcow),
                                 'x86_64', 'generic'], capture_output=True, timeout=5, check=True)
        self.assertEqual(json.loads(result.stdout), {'home_archive': self.home})
        qcow.write_bytes(b'substituted image')
        with self.assertRaises(ValueError):
            media.validate(qcow, 'x86_64', 'generic')
        for bad in ({'applications': self.cap}, {'home_archive': dict(self.home, schema=True)},
                    {'home_archive': self.home, 'unknown': True}):
            with self.assertRaises(ValueError):
                media.validate_consumers(bad)

    def test_bounded_native_command(self):
        self.assertEqual(c.invoke(['/bin/sh', '-c', 'printf ok']), b'ok')
        for command, limit, timeout in [(['/bin/sh', '-c', 'exit 7'], 4096, 2),
                                        (['/bin/sh', '-c', 'head -c 4097 /dev/zero'], 4096, 2),
                                        (['/bin/sh', '-c', 'sleep 60 & exit 0'], 4096, 0.1)]:
            with self.assertRaises(ValueError):
                c.invoke(command, limit, timeout)


if __name__ == '__main__':
    unittest.main()
