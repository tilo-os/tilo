"""Isolated production ordering/staging tests. No Cargo, images or signed fixtures."""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('capture_pipeline', HERE / 'prepare-capture-release.py')
pipeline = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pipeline)


class PrerequisiteBase(unittest.TestCase):
    def setUp(self):
        path = pipeline.ROOT / 'os/scripts/installer-receive-fixture/prerequisite-image.py'
        spec = importlib.util.spec_from_file_location('capture_prerequisite_test', path)
        self.prerequisites = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.prerequisites)

    def test_actual_recipe_and_provenance_use_selected_immutable_base(self):
        base = 'registry.fedoraproject.org/fedora@sha256:' + 'b' * 64
        with tempfile.TemporaryDirectory() as scratch:
            context = Path(scratch) / 'context'
            manifest = self.prerequisites.assemble(context, base)
            self.assertEqual(manifest['base'], base)
            self.assertEqual((context / 'Containerfile').read_text().splitlines()[0],
                             'FROM ' + base)
            recorded = json.loads((context / 'source-inputs.json').read_text())
            self.assertEqual(recorded['base'], base)
            self.assertFalse(recorded['qualification'])
            self.assertFalse(recorded['candidate_executables_included'])
            for name in ('tilo-staging-handoff', 'tilo-install-handoff-copy.py', 'tilo-firstboot-files'):
                self.assertNotIn('os/usr/libexec/' + name, recorded['sources'])
                self.assertFalse((context / 'rootfs/usr/libexec' / name).exists())

    def test_mutable_or_injected_base_refused_before_allocating_context(self):
        with tempfile.TemporaryDirectory() as scratch:
            context = Path(scratch) / 'context'
            for base in ('fedora:44', 'sha256:' + 'a' * 63,
                         'sha256:' + 'a' * 64 + '\\nRUN true'):
                with self.subTest(base=base), self.assertRaises(ValueError):
                    self.prerequisites.assemble(context, base)
                self.assertFalse(context.exists())


class BuildOrder(unittest.TestCase):
    def test_missing_authority_refuses_before_any_build(self):
        with tempfile.TemporaryDirectory() as scratch, patch.object(pipeline, 'run') as run:
            with self.assertRaisesRegex(ValueError, 'TILO_VERSION'):
                pipeline.prepare(Path(scratch) / 'output', 'host-media-writer', {})
            run.assert_not_called()
            self.assertFalse((Path(scratch) / 'output').exists())

    def exercise(self, reject=False, build_only=False):
        with tempfile.TemporaryDirectory() as scratch:
            output = Path(scratch) / 'output'
            calls = []
            image = 'sha256:' + 'a' * 64
            env = dict(TILO_VERSION='0.1.0', TILO_CAPTURE_RUNTIME_IDENTITY='/operator/runtime',
                       TILO_CAPTURE_SERVICE_IDENTITY='/operator/service',
                       TILO_CAPTURE_QUALIFIER_BASE=image, TILO_CAPTURE_SOURCE_ASSETS='/operator/assets',
                       TILO_CAPTURE_SIGNING_PURPOSE='developer-isolated')

            def assemble(context, base):
                self.assertEqual(base, env['TILO_CAPTURE_QUALIFIER_BASE'])
                context.mkdir()
                (context / 'rootfs/etc/tilo').mkdir(parents=True)
                (context / 'rootfs/usr/libexec').mkdir(parents=True)
                (context / 'rootfs/usr/bin').mkdir(parents=True)
                (context / 'rootfs/etc/tilo/runtime-bundle-trusted-publishers.txt').write_text('fixture\n')

            def prepare_browser(browser_output, sidecar, pins, runtime_identity, app_identity):
                browser_output.mkdir()
                (browser_output / 'cef').mkdir()
                for name in ('cef.tapp', 'browser.tapp'):
                    (browser_output / name).write_bytes(b'signed fixture')
                (sidecar / 'runtime/sdk').mkdir(parents=True)
                (context_host := output / 'mock-host').write_bytes(b'host')
                return {'runtime_version': 'sdk', 'runtime_publisher': 'a' * 64}, 'cef', 'a' * 64, 'b' * 64

            def fixed_module(path, name):
                return SimpleNamespace(prepare=prepare_browser, stage=Mock(return_value='RUN true\n')) if name == 'capture_browser' else SimpleNamespace(assemble=assemble)

            qualifier = SimpleNamespace(source_assets=Mock(), BINDINGS={},
                admit_cohort=Mock(return_value={'roles': {'host': {'path': str(output / 'mock-host'), 'sha256': 'a' * 64}}}),
                bound_file=Mock(), fixed_module=fixed_module)

            def run(command, selected):
                command = list(map(str, command))
                calls.append((command, dict(selected)))
                if command[:2] == ['podman', 'build']:
                    Path(command[command.index('--iidfile') + 1]).write_text(image)
                if 'qualify-capture-release.py' in command[1]:
                    if reject:
                        raise subprocess.CalledProcessError(1, command)

            with patch.object(pipeline, 'load_qualifier', return_value=qualifier), patch.object(pipeline, 'run', side_effect=run):
                if reject:
                    with self.assertRaises(subprocess.CalledProcessError):
                        pipeline.prepare(output, 'host-media-writer', env)
                    self.assertFalse((output / 'environment.json').exists())
                    return
                result = pipeline.prepare(output, 'host-media-writer', env, build_only=build_only)
            commands = [row[0] for row in calls]
            if build_only:
                self.assertFalse(any(command[:2] == ['podman', 'build'] for command in commands))
                self.assertFalse(any('qualify-capture-release.py' in command[1] for command in commands))
                self.assertNotIn('TILO_CAPTURE_QUALIFIER_IMAGE', result)
                self.assertEqual(result['TILO_CAPTURE_SIGNING_PURPOSE'], 'developer-isolated')
                self.assertEqual(json.loads((output / 'build-inputs.json').read_text()), result)
                self.assertFalse((output / 'environment.json').exists())
                self.assertEqual(commands[-1][1], 'cohort')
                return
            worker = next(i for i, cmd in enumerate(commands) if '--build-only' in cmd)
            assembly = next(i for i, cmd in enumerate(commands) if 'assemble' in cmd)
            host = next(i for i, cmd in enumerate(commands) if '--bin' in cmd and cmd[-1] == 'imager/host-media-writer')
            cohort = next(i for i, cmd in enumerate(commands) if 'cohort' in cmd)
            qualification = len(commands) - 1
            self.assertLess(worker, assembly)
            self.assertLess(assembly, host)
            self.assertLess(host, cohort)
            self.assertLess(cohort, qualification)
            self.assertIn('--with-writer', commands[worker])
            prerequisites = commands[worker - 1]
            for package, name in (('tilo-install-helpers', 'tilo-staging-handoff'),
                                  ('tilo-install-helpers', 'tilo-install-handoff-copy'),
                                  ('tilo-firstboot-files', 'tilo-firstboot-files')):
                self.assertIn(package, prerequisites)
                self.assertIn(name, prerequisites)
            self.assertIn('tilo-firstboot-home-seeds', commands[host])
            self.assertIn('tilo-installer-prepared-setup-extract', commands[host])
            self.assertNotIn('TILO_CAPTURE_PINS', calls[worker][1])
            self.assertEqual(calls[host][1]['TILO_CAPTURE_PINS'], str(output / 'pins.json'))
            self.assertIn('--source-assets', commands[qualification])
            self.assertNotIn('--source-backend-only', commands[qualification])
            self.assertEqual(result['TILO_CAPTURE_QUALIFIER_IMAGE'], image)
            self.assertEqual(json.loads((output / 'environment.json').read_text()), result)

    def test_actual_producers_precede_host_and_mandatory_full_gate(self):
        self.exercise()

    def test_build_only_exposes_inputs_without_claiming_qualification(self):
        self.exercise(build_only=True)

    def test_failed_full_gate_exposes_no_packaging_environment(self):
        self.exercise(reject=True)

    def test_release_workflow_orders_both_hosts_and_media_after_capture(self):
        workflow = (pipeline.ROOT / '.forgejo/workflows/release-imager.yml').read_text()
        media = workflow[workflow.index('  medium:'):workflow.index('  catalog:')]
        self.assertLess(media.index('prepare-capture-release.py'), media.index('layer-capture-install-medium.py'))
        self.assertLess(media.index('layer-capture-install-medium.py'), media.index('./os/dev/build-custom-installer.sh'))
        self.assertLess(media.index('./os/dev/build-custom-installer.sh'), media.index('package-install-medium.sh'))
        linux = workflow[workflow.index('  linux:'):workflow.index('  windows:')]
        self.assertLess(linux.index('prepare-capture-release.py'), linux.index('package-host.sh'))
        self.assertLess(linux.index('prepare-capture-release.py'), linux.index('package-flatpak.sh'))


class MediumLayer(unittest.TestCase):
    def test_installed_platform_paths_follow_full_gate_and_keep_observed_bytes(self):
        spec = importlib.util.spec_from_file_location('capture_medium', HERE / 'layer-capture-install-medium.py')
        medium = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(medium)
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            binaries = root / 'target/release'
            binaries.mkdir(parents=True)
            for name in ('imager', 'tilo-installer', 'fs'):
                (binaries / name).write_bytes(name.encode())
            calls = []
            qualifier = SimpleNamespace(BINDINGS={'fs': '/usr/bin/fs', 'installer': '/usr/bin/tilo-installer'}, bound_file=Mock(),
                admit_cohort=Mock(return_value={'roles': {
                    'fs': {'path': str(binaries / 'fs'), 'sha256': 'f' * 64},
                    'installer': {'path': str(binaries / 'tilo-installer'), 'sha256': 'a' * 64}}}),
                fixed_module=Mock(return_value=SimpleNamespace(stage=Mock(return_value='RUN normal receiving install\n'))))

            def run(command, env):
                command = list(map(str, command))
                calls.append(command)
                if command[0] == 'bash':
                    self.assertEqual(Path(command[-1]).read_bytes(), b'imager')
                    self.assertFalse((Path(command[-2]) / 'bin/fs').exists())
                elif command[:2] == ['podman', 'build']:
                    context = Path(command[-1])
                    self.assertEqual((context / 'rootfs/usr/bin/fs').read_bytes(), b'fs')
                    self.assertEqual((context / 'rootfs/usr/bin/tilo-installer').read_bytes(), b'tilo-installer')
                    self.assertIn('COPY rootfs/ /', (context / 'Containerfile').read_text())

            settings = {name: '/measured/' + name for name in (
                'TILO_CAPTURE_PINS', 'TILO_CAPTURE_SIDECAR', 'TILO_CAPTURE_QUALIFICATION_COHORT',
                'TILO_CAPTURE_QUALIFIER_IMAGE', 'TILO_CAPTURE_SOURCE_ASSETS', 'TILO_CAPTURE_BROWSER_RELEASE')}
            with patch.object(medium.release, 'ROOT', root), patch.object(medium.release, 'run', side_effect=run), patch.object(medium.release, 'load_qualifier', return_value=qualifier):
                medium.layer(settings, 'sha256:' + 'b' * 64, root / 'iid', {})
            self.assertEqual([command[0] for command in calls], ['bash', 'podman'])
            qualifier.admit_cohort.assert_called_once()
            self.assertEqual(qualifier.bound_file.call_count, 4)
            stage = qualifier.fixed_module.return_value.stage
            stage.assert_called_once()
            self.assertFalse(stage.call_args.kwargs['qualification'])

    def test_developer_keys_cannot_escape_to_an_ordinary_medium(self):
        spec = importlib.util.spec_from_file_location('capture_medium_developer', HERE / 'layer-capture-install-medium.py')
        medium = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(medium)
        settings = {name: '/measured/' + name for name in (
            'TILO_CAPTURE_PINS', 'TILO_CAPTURE_SIDECAR', 'TILO_CAPTURE_QUALIFICATION_COHORT',
            'TILO_CAPTURE_QUALIFIER_IMAGE', 'TILO_CAPTURE_SOURCE_ASSETS', 'TILO_CAPTURE_BROWSER_RELEASE')}
        for purpose in ('developer-qualification', 'developer-isolated'):
            with self.subTest(purpose=purpose), patch.object(medium.release, 'run') as run:
                settings['TILO_CAPTURE_SIGNING_PURPOSE'] = purpose
                with self.assertRaisesRegex(ValueError, 'confined'):
                    medium.layer(settings, 'sha256:' + 'b' * 64, Path('/unallocated/iid'), {})
                run.assert_not_called()


class NativePlatformArchive(unittest.TestCase):
    def test_qualified_native_helpers_and_sidecar_have_canonical_root_paths(self):
        import tarfile
        spec = importlib.util.spec_from_file_location('capture_platform', HERE / 'package-capture-platform.py')
        platform = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(platform)
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            roles = {}
            for role in ('prepared_helper', 'credential_helper', 'home_seeds', 'prepared_extract'):
                path = root / role
                path.write_bytes(role.encode())
                roles[role] = {'path': str(path), 'sha256': 'a' * 64}
            host = root / 'host'
            host.write_bytes(b'host')
            qualifier = SimpleNamespace(BINDINGS={
                'prepared_helper': '/usr/libexec/tilo-prepared-setup',
                'credential_helper': '/usr/libexec/tilo-account-credential',
                'home_seeds': '/usr/libexec/tilo-firstboot-home-seeds',
                'prepared_extract': '/usr/libexec/tilo-installer-prepared-setup-extract'},
                bound_file=Mock(), admit_cohort=Mock(return_value={'roles': roles}))

            def gate(command, env):
                self.assertIn('stage-capture-sidecar.sh', str(command[1]))
                prefix = Path(command[-2])
                sidecar = prefix / 'share/tilo-imager/capture/service'
                sidecar.mkdir(parents=True)
                (sidecar / 'home-export.tapp').write_bytes(b'isolated sidecar')

            output = root / 'platform.tar.gz'
            with patch.object(platform.release, 'load_qualifier', return_value=qualifier), patch.object(platform.release, 'run', side_effect=gate) as run:
                platform.package(output, host, {'TILO_CAPTURE_QUALIFICATION_COHORT': '/observed/cohort'})
            run.assert_called_once()
            with tarfile.open(output) as archive:
                for name, value in (
                    ('usr/bin/tilo-imager', b'host'),
                    ('usr/libexec/tilo-prepared-setup', b'prepared_helper'),
                    ('usr/libexec/tilo-account-credential', b'credential_helper'),
                    ('usr/libexec/tilo-firstboot-home-seeds', b'home_seeds'),
                    ('usr/libexec/tilo-installer-prepared-setup-extract', b'prepared_extract'),
                    ('usr/share/tilo-imager/capture/service/home-export.tapp', b'isolated sidecar')):
                    member = archive.getmember(name)
                    self.assertEqual((member.uid, member.gid), (0, 0))
                    self.assertEqual(archive.extractfile(member).read(), value)
                self.assertEqual(archive.getmember('usr/libexec/tilo-prepared-setup').mode, 0o755)


class StageGate(unittest.TestCase):
    def test_required_capture_cannot_be_silently_disabled(self):
        with tempfile.TemporaryDirectory() as scratch:
            env = {key: value for key, value in os.environ.items()
                   if not key.startswith('TILO_CAPTURE_')}
            env['TILO_REQUIRE_CAPTURE'] = '1'
            result = subprocess.run(['bash', str(HERE / 'stage-capture-sidecar.sh'),
                                     '/missing/host', scratch], env=env, capture_output=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(list(Path(scratch).iterdir()), [])

    def test_copy_occurs_only_after_full_execution_and_staged_bytes_are_readmitted(self):
        for reject in (False, True):
            with self.subTest(reject=reject), tempfile.TemporaryDirectory() as scratch:
                root = Path(scratch)
                packaging = root / 'apps/imager/packaging'
                packaging.mkdir(parents=True)
                shutil.copyfile(HERE / 'stage-capture-sidecar.sh', packaging / 'stage-capture-sidecar.sh')
                log = root / 'log'
                # Test doubles record the production shell's invocation order.
                (packaging / 'admit-capture-sidecar.py').write_text(
                    'import os,sys\nfrom pathlib import Path\n'
                    'with open(os.environ["STAGE_TEST_LOG"],"a") as out: out.write("admit:"+sys.argv[sys.argv.index("--sidecar")+1]+"\\n")\n')
                (packaging / 'qualify-capture-release.py').write_text(
                    'import os,sys\nfrom pathlib import Path\n'
                    'assert "--source-assets" in sys.argv\n'
                    'assert not Path(os.environ["STAGE_TEST_DEST"]).exists()\n'
                    'with open(os.environ["STAGE_TEST_LOG"],"a") as out: out.write("qualify\\n")\n'
                    'sys.exit(int(os.environ["STAGE_TEST_REJECT"]))\n')
                host = root / 'host'
                host.write_bytes(b'isolated host')
                sidecar = root / 'sidecar'
                sidecar.mkdir()
                (sidecar / 'subject').write_bytes(b'isolated sidecar')
                prefix = root / 'package'
                destination = prefix / 'share/tilo-imager/capture'
                env = dict(os.environ, TILO_CAPTURE_PINS=str(root / 'pins'),
                           TILO_CAPTURE_SIDECAR=str(sidecar), TILO_CAPTURE_SOURCE_ASSETS='/official/assets',
                           STAGE_TEST_LOG=str(log), STAGE_TEST_DEST=str(destination), STAGE_TEST_REJECT=str(int(reject)))
                result = subprocess.run(['bash', str(packaging / 'stage-capture-sidecar.sh'),
                                         str(host), str(prefix)], env=env, capture_output=True)
                self.assertEqual(result.returncode == 0, not reject, result.stderr)
                lines = log.read_text().splitlines()
                self.assertEqual(lines[:2], ['admit:' + str(sidecar), 'qualify'])
                if reject:
                    self.assertFalse(destination.exists())
                else:
                    self.assertEqual(lines[2:], ['admit:' + str(destination)])
                    self.assertEqual((destination / 'subject').read_bytes(), b'isolated sidecar')


if __name__ == '__main__':
    unittest.main()
