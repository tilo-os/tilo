"""Production qualifier boundaries and real owned subprocess retirement; no native/image execution."""
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('qualifier', Path(__file__).with_name('qualify-capture-release.py'))
qualifier = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qualifier)


class StagedSourceClosure(unittest.TestCase):
    def test_actual_local_imports_and_control_client_are_staged(self):
        import ast
        files = set(qualifier.FIXTURE_FILES)
        directories = ('os/scripts/installer-source-fixture',
                       'os/scripts/installer-receive-fixture')
        for relative in files:
            if not relative.endswith('.py'):
                continue
            tree = ast.parse((qualifier.ROOT / relative).read_text())
            for node in ast.walk(tree):
                names = ([alias.name for alias in node.names] if isinstance(node, ast.Import)
                         else [node.module] if isinstance(node, ast.ImportFrom) else [])
                for name in names:
                    if not name:
                        continue
                    for directory in directories:
                        dependency = directory + '/' + name.split('.')[0] + '.py'
                        if (qualifier.ROOT / dependency).is_file():
                            self.assertIn(dependency, files, (relative, name))
        self.assertIn('scripts/headless-examples/_control.py', files)
        prerequisites = qualifier.fixed_module(qualifier.ROOT /
            'os/scripts/installer-receive-fixture/prerequisite-image.py', 'test_prerequisites')
        self.assertLessEqual(set(prerequisites.ASSETS), files)
        for name in ('tilo-firstboot-home-seeds.py', 'tilo-installer-prepared-setup-extract.py',
                     'tilo-staging-handoff', 'tilo-install-handoff-copy.py', 'tilo-firstboot-files'):
            self.assertNotIn('os/usr/libexec/' + name, files)
            self.assertNotIn('os/usr/libexec/' + name, prerequisites.ASSETS)

    def test_production_stage_copies_and_binds_whole_closure(self):
        sources = {name: qualifier.facts(qualifier.ROOT / name)['sha256']
                   for name in (*qualifier.FIXTURE_FILES, *qualifier.PACKAGE_FILES)}
        with tempfile.TemporaryDirectory() as temporary:
            stage = Path(temporary).resolve()
            qualifier.stage_sources(stage, sources)
            self.assertEqual(json.loads((stage / 'source-inputs.json').read_text()), sources)
            for name, digest in sources.items():
                self.assertEqual(qualifier.facts(stage / name)['sha256'], digest)
                self.assertEqual((stage / name).stat().st_mode & 0o777, 0o644)
        for name in ('os/scripts/installer-source-fixture/secret_owner.py',
                     'scripts/headless-examples/_control.py'):
            with tempfile.TemporaryDirectory() as temporary:
                altered = dict(sources, **{name: '0' * 64})
                with self.assertRaises(ValueError):
                    qualifier.stage_sources(Path(temporary).resolve(), altered)

    def test_unpinned_official_manifest_and_runtime_overlays_refuse(self):
        with tempfile.TemporaryDirectory() as temporary:
            manifest = Path(temporary).resolve() / 'assets.json'
            manifest.write_text('{"schema":1,"assets":{}}')
            with self.assertRaisesRegex(ValueError, 'official public asset manifest'):
                qualifier.source_assets(manifest)
        for path in ('/usr/bin', '/owned/candidate', '/run/assets', '/var/../etc',
                     '/srv/tilo', '/srv/tilo/tasks', '/srv/tilo/tasks-other/assets',
                     '/srv/tilo/p/source'):
            with self.assertRaisesRegex(ValueError, 'mount escapes'):
                qualifier.asset_mounts({'assets': {'app': {'source_root': path}}, 'archives': {}})

    def test_asset_admission_measures_bytes_before_mount_coordinates(self):
        from unittest.mock import Mock, patch
        assets = {'assets': {}, 'archives': {}}
        helper = Mock()
        helper.verify.side_effect = ValueError('public file digest differs')
        with patch.object(qualifier, 'document', return_value=assets), \
                patch.object(qualifier, 'fixed_module', return_value=helper):
            with self.assertRaisesRegex(ValueError, 'public file digest differs'):
                qualifier.source_assets(Path('/unused/manifest'))
        helper.verify.assert_called_once_with(assets)
        helper.validate.assert_not_called()

    def test_durable_public_task_asset_coordinates_are_admitted(self):
        path = '/srv/tilo/tasks/installer-public-source/target/capture-public-assets/app'
        self.assertEqual(qualifier.asset_mounts({
            'assets': {'app': {'source_root': path}}, 'archives': {}}),
            ['--volume', path + ':' + path + ':ro'])

    def test_actual_source_accepts_receivers_keyword_mode_without_changing_default_context(self):
        import ast
        source_path = qualifier.ROOT / 'os/scripts/installer-source-fixture/source.py'
        saved_path = list(sys.path)
        try:
            source = qualifier.fixed_module(source_path, 'actual_source_keyword_boundary')
            with self.assertRaisesRegex(RuntimeError, 'mode must be boolean'):
                source.run('/unused', '/unused', lambda context: context, service_qualification=1)
            with self.assertRaisesRegex(RuntimeError, 'exact authenticated candidate scope'):
                source.run('/unused', '/unused', lambda context: context, service_qualification=True)
            with self.assertRaisesRegex(RuntimeError, 'ordinary source'):
                source.run('/unused', '/unused', lambda context: context, service_candidate={})
        finally:
            sys.path[:] = saved_path
        tree = ast.parse(source_path.read_text())
        run = next(node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name == 'run')
        self.assertEqual([argument.arg for argument in run.args.kwonlyargs],
                         ['service_qualification', 'service_candidate', 'service_request'])
        self.assertIs(run.args.kw_defaults[0].value, False)
        self.assertIsNone(run.args.kw_defaults[2].value)
        branch = next(node for node in ast.walk(run) if isinstance(node, ast.If)
                      and isinstance(node.test, ast.Name) and node.test.id == 'service_qualification'
                      and "context['service_qualification']" in ast.unparse(node))
        self.assertIn("context['service_qualification'] = True", ast.unparse(branch))

    def test_asset_preparation_precedes_actual_source_producer(self):
        runner = qualifier.CONTAINER_RUNNER
        self.assertLess(runner.index('qualifier.prepare_source(candidate)'),
                        runner.index("str(candidate/qualifier.TARGET_FIXTURE)"))


class NativeQualificationBoundary(unittest.TestCase):
    def test_loop_or_reused_store_contract_cannot_start_native_qualification(self):
        from types import SimpleNamespace
        from unittest.mock import patch
        provenance = Path('/var/tmp/native-store-private-0123456789abcdef/provenance.json')
        for paths in ([], [provenance], [provenance, provenance],
                      [Path('relative/provenance.json'), Path('/other/provenance.json')]):
            args = SimpleNamespace(source_backend_only=True, case=None,
                                   native_store_provenance=paths)
            with self.subTest(paths=paths), patch.object(qualifier.os, 'geteuid', return_value=1000), \
                    patch.object(qualifier, 'execute') as execute:
                with self.assertRaisesRegex(ValueError, 'loops cannot qualify'):
                    qualifier.run_native(args, {}, Path('/unused'))
                execute.assert_not_called()

    def test_pivot_admission_refuses_missing_loop_and_outside_fixture_paths(self):
        image = qualifier.fixed_module(Path(__file__).with_name('capture-native-image.py'),
                                       'actual_native_image_boundary')
        for path in (None, Path('/dev/loop0'), Path('relative/provenance.json'),
                     Path('/usr/provenance.json')):
            with self.subTest(path=path), self.assertRaisesRegex(ValueError, 'loops cannot qualify'):
                image.admit_native_stores(Path('/unused'), path)

    def test_pivot_requires_actual_device_validation_before_mounting(self):
        from unittest.mock import Mock, patch
        image = qualifier.fixed_module(Path(__file__).with_name('capture-native-image.py'),
                                       'actual_native_device_boundary')
        helper = Mock()
        rows = [{'name': 'a'}, {'name': 'b'}]
        helper.provenance.return_value = rows
        helper.device.side_effect = ValueError('expected exactly one serial-matching virtio disk')
        path = Path('/var/tmp/native-store-private-0123456789abcdef/provenance.json')
        with patch.object(image.importlib.util, 'module_from_spec', return_value=helper), \
                patch.object(image.importlib.util, 'spec_from_file_location') as spec:
            spec.return_value.loader.exec_module = Mock()
            with self.assertRaisesRegex(ValueError, 'serial-matching virtio'):
                image.admit_native_stores(Path('/staged'), path)
        helper.provenance.assert_called_once_with(path.parent, path)
        helper.device.assert_called_once_with(rows[0])

    def test_native_runner_preserves_explicit_virtio_contract_without_loop_fallback(self):
        compile(qualifier.CONTAINER_RUNNER, '<actual native runner>', 'exec')
        self.assertNotIn('TILO_NATIVE_STORE_AUTOCLEAR', qualifier.CONTAINER_RUNNER)
        self.assertIn('TILO_NATIVE_STORE_VIRTIO_PROVENANCE=str(provenance)',
                      qualifier.CONTAINER_RUNNER)

    def receiving_observation(self):
        return {'schema': 1, 'status': 'completed', 'shared_files_readwrite': True,
                'writer_shared_file_edit': True, 'apps_data_and_launch': True,
                'protected_cloud_custody': True, 'cloud_fs_and_files': True,
                'identity_headless_lifecycle': True, 'owned_resources_retired': True,
                'service_sessions': dict(schema=2, service_session_protocol=2,
                    receiving_route_qualification_sha256='a' * 64,
                    **{condition: True for condition in qualifier.SERVICE_CONDITIONS})}

    def test_actual_command_stream_requires_whole_closed_receiving_observation(self):
        value = self.receiving_observation()
        with tempfile.TemporaryDirectory() as temporary:
            code, output = qualifier.execute([sys.executable, '-c',
                'print(' + repr(json.dumps(value)) + ')'], Path(temporary) / 'log', 3)
            self.assertEqual(code, 0)
            self.assertEqual(qualifier.target_completion(output), value)
        for data in (b'progress\n' + output, output + b'progress\n', output + output):
            with self.assertRaises((ValueError, json.JSONDecodeError)):
                qualifier.target_completion(data)

    def test_missing_false_wrong_type_and_extra_receiving_claims_refuse(self):
        value = self.receiving_observation()
        for key in value:
            missing = dict(value)
            del missing[key]
            with self.assertRaises(ValueError):
                qualifier.target_completion(json.dumps(missing).encode())
        for key in value:
            if key not in ('schema', 'status'):
                for wrong in (False, 1, 'true'):
                    with self.assertRaises(ValueError):
                        qualifier.target_completion(json.dumps(dict(value, **{key: wrong})).encode())
        for altered in (dict(value, schema=True), dict(value, status='ready'), dict(value, extra=True)):
            with self.assertRaises(ValueError):
                qualifier.target_completion(json.dumps(altered).encode())
        with self.assertRaises(ValueError):
            qualifier.target_completion(b'x' * 4097)

    def test_full_native_completion_requires_every_revocation_and_isolation_observation(self):
        for condition in qualifier.SERVICE_CONDITIONS:
            value = self.receiving_observation()
            value['service_sessions'][condition] = False
            with self.assertRaisesRegex(ValueError, 'import/revocation'):
                qualifier.target_completion(json.dumps(value).encode())
        value = self.receiving_observation()
        del value['service_sessions']['revocation_verified']
        with self.assertRaises(ValueError):
            qualifier.target_completion(json.dumps(value).encode())

    def test_duplicate_input_field_is_rejected(self):
        with self.assertRaisesRegex(ValueError, 'duplicate'):
            json.loads('{"role":"host","role":"imager_tests"}', object_pairs_hook=qualifier.unique)

    def test_real_command_exit_is_observed_and_output_stays_private(self):
        with tempfile.TemporaryDirectory() as temporary:
            log = Path(temporary) / 'private.log'
            code, data = qualifier.execute([sys.executable, '-c',
                'import sys;print("fixture");sys.exit(7)'], log, 3)
            self.assertEqual(code, 7)
            self.assertEqual(data, b'fixture\n')
            self.assertEqual(log.stat().st_mode & 0o777, 0o600)

    def test_actual_timeout_reaps_held_process_before_return(self):
        with tempfile.TemporaryDirectory() as temporary:
            log = Path(temporary) / 'private.log'
            with self.assertRaisesRegex(ValueError, 'deadline'):
                qualifier.execute([sys.executable, '-u', '-c',
                    'import os,time;print(os.getpid());time.sleep(30)'], log, .2)
            pid = int(log.read_text().strip())
            self.assertFalse(Path('/proc', str(pid)).exists())

    def test_redirected_candidate_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            target = root / 'candidate'
            target.write_bytes(b'actual fixture bytes')
            target.chmod(0o600)
            link = root / 'redirected'
            link.symlink_to(target)
            with self.assertRaisesRegex(ValueError, 'redirected'):
                qualifier.facts(link)

    def test_live_file_growth_is_bounded_on_the_same_held_read(self):
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary).resolve() / 'growing'
            path.write_bytes(b'a')
            path.chmod(0o600)
            original_read = os.read
            def grow(fd, count):
                with path.open('ab') as writer:
                    writer.write(b'b' * 20)
                return original_read(fd, count)
            with patch.object(qualifier.os, 'read', side_effect=grow):
                with self.assertRaisesRegex(ValueError, 'grew beyond bound'):
                    qualifier.read_input(path, 10, True)

    def test_document_decodes_its_held_bytes_without_path_reopen(self):
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary).resolve() / 'record'
            path.write_text('{"schema":1}')
            path.chmod(0o600)
            with patch.object(Path, 'read_bytes', side_effect=AssertionError('path reopened')):
                self.assertEqual(qualifier.document(path), {'schema': 1})

    def test_role_requires_compiler_source_pair_bindings(self):
        with self.assertRaisesRegex(ValueError, 'role shape'):
            qualifier.validate_role(Path('/unused'), 'host',
                {'path': '/old-successful-artifact', 'sha256': 'a' * 64}, {})

    def test_actual_cli_rejects_saved_ready_receipt_before_native_execution(self):
        import subprocess
        with tempfile.TemporaryDirectory() as temporary:
            receipt = Path(temporary) / 'saved-ready.json'
            receipt.write_text('{"schema":1,"publication_qualified":true}')
            receipt.chmod(0o600)
            # The timeout only bounds a hang: under a loaded suite a fresh
            # interpreter's imports alone can take seconds before it refuses.
            result = subprocess.run([sys.executable, str(Path(qualifier.__file__)),
                '--host', sys.executable, '--pins', str(receipt), '--sidecar', temporary,
                '--arch', 'x86_64', '--cohort', str(receipt), '--image', 'sha256:' + 'a' * 64],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=60)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(result.stdout, b'')
            self.assertIn(b'qualification refused', result.stderr)


if __name__ == '__main__':
    unittest.main()
