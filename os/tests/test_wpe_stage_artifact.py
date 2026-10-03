import copy
from contextlib import nullcontext
import importlib.util
import json
from pathlib import Path
import tempfile
import subprocess
from types import SimpleNamespace
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location('wpe_artifact',
    Path(__file__).resolve().parents[1] / 'dev/wpe-stage-artifact.py')
wpe = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(wpe)


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


class WpeArtifactTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.repo = Path(self.temporary.name)
        (self.repo / 'os/dev').mkdir(parents=True)
        self.assets = self.repo / 'assets'
        self.assets.mkdir()
        script = []
        for variable in ('LIBWPE', 'FDO', 'WPEWK'):
            name = variable.lower() + '-1.tar.xz'
            data = variable.encode()
            (self.assets / name).write_bytes(data)
            script.extend((f'{variable}={name}', f'{variable}_SHA256={wpe.sha(data)}'))
        (self.repo / 'os/dev/build-wpe.sh').write_text('\n'.join(script) + '\n')
        (self.repo / 'os/Containerfile.wpe').write_text('FROM scratch\n')
        (self.repo / 'os/Containerfile').write_text('FROM scratch AS os\n')
        self.buildroot = 'registry.tilo.so/tilo/tilo-wpe-buildroot@sha256:' + '1' * 64
        self.inputs = self.dependencies()
        self.config = wpe.canonical({'architecture': 'amd64', 'os': 'linux'})
        self.identity = 'sha256:' + wpe.sha(self.config)
        self.manifest = wpe.canonical({'schemaVersion': 2,
            'mediaType': 'application/vnd.oci.image.manifest.v1+json',
            'config': {'digest': self.identity}, 'layers': [{'digest': 'sha256:' + '2' * 64}]})
        self.reference = wpe.repository('x86_64') + '@sha256:' + wpe.sha(self.manifest)
        self.receipt = dict(schema=1, purpose=wpe.PURPOSE, production_signed=False,
            inputs=self.inputs, dependency_key=wpe.sha(wpe.canonical(self.inputs)),
            reference=self.reference, config_digest=self.identity, payload={'proof': 'hash'},
            build=dict(jobs=6, revision='a' * 40, run_id='42', attempt='1',
                       source_completed_stage_id=self.identity))

    def dependencies(self):
        return wpe.dependency_inputs(self.repo, self.assets, 'x86_64', '0', self.buildroot)

    def test_dependency_key(self):
        with self.subTest(phase="dependency_key_ignores_unrelated_sources_and_records_actual_assets"):
            (self.repo / 'app.rs').write_text('new app')
            self.assertEqual(self.dependencies(), self.inputs)
            (self.repo / 'os/Containerfile').write_text('FROM unrelated-os-edit\n')
            self.assertEqual(self.dependencies(), self.inputs)
            (self.repo / 'os/Containerfile.wpe').write_text('FROM different-wpe-recipe\n')
            self.assertNotEqual(self.dependencies(), self.inputs)
            (self.assets / 'fdo-1.tar.xz').write_text('tampered')
            with self.assertRaises(ValueError):
                self.dependencies()
        restart_fixture(self)
        with self.subTest(phase="mutable_buildroot_and_duplicate_pins_rejected"):
            with self.assertRaises(ValueError):
                wpe.dependency_inputs(self.repo, self.assets, 'x86_64', '0', self.buildroot.split('@')[0] + ':latest')
            with (self.repo / 'os/dev/build-wpe.sh').open('a') as stream:
                stream.write('LIBWPE=other.tar.xz\n')
            with self.assertRaises(ValueError):
                self.dependencies()
        restart_fixture(self)
        with self.subTest(phase="receipt_jobs_are_provenance_not_dependency_key"):
            for jobs in (2, 6):
                value = copy.deepcopy(self.receipt)
                value['build']['jobs'] = jobs
                wpe.validate_receipt(value, self.inputs)
            for field, value in [('purpose', 'production'), ('production_signed', True),
                                 ('dependency_key', 'f' * 64), ('reference', self.reference.split('@')[0] + ':latest')]:
                with self.subTest(field=field), self.assertRaises(ValueError):
                    wpe.validate_receipt(self.receipt | {field: value}, self.inputs)
            for field, value in [('architecture', 'aarch64'), ('epoch', '1'),
                                 ('wpe_buildroot', self.buildroot[:-1] + '2')]:
                with self.subTest(field=field), self.assertRaises(ValueError):
                    wpe.validate_receipt(self.receipt, self.inputs | {field: value})
        restart_fixture(self)
        with self.subTest(phase="source_revision_and_dirty_checkout_rejected"):
            revision = 'a' * 40
            good = [revision.encode(), str(self.repo).encode(), b'tracked', b'']
            with patch.object(wpe, 'command', side_effect=good):
                wpe.verify_source(self.repo, revision)
            with patch.object(wpe, 'command', return_value=b'wrong'):
                with self.assertRaises(ValueError):
                    wpe.verify_source(self.repo, revision)
            with patch.object(wpe, 'command', side_effect=good[:3] + [b' M file']):
                with self.assertRaises(ValueError):
                    wpe.verify_source(self.repo, revision)

    def test_raw_manifest_and_registry(self):
        with self.subTest(phase="raw_manifest_config_and_architecture_are_independently_bound"):
            self.assertEqual(wpe.manifest_identity(self.manifest, self.config, self.reference, 'x86_64'), self.identity)
            for raw, config, reference, arch in (
                    (self.manifest + b' ', self.config, self.reference, 'x86_64'),
                    (self.manifest, self.config + b' ', self.reference, 'x86_64'),
                    (self.manifest, self.config, self.reference, 'aarch64')):
                with self.assertRaises(ValueError):
                    wpe.manifest_identity(raw, config, reference, arch)
            index = wpe.canonical({'schemaVersion': 2, 'manifests': []})
            with self.assertRaises(ValueError):
                wpe.manifest_identity(index, self.config,
                    wpe.repository('x86_64') + '@sha256:' + wpe.sha(index), 'x86_64')
        restart_fixture(self)
        with self.subTest(phase="registry_uses_raw_config_and_explicit_tls"):
            with patch.object(wpe, 'command', side_effect=[self.manifest, self.config]) as command:
                self.assertEqual(wpe.registry_identity(self.reference, 'x86_64'), self.identity)
            self.assertEqual(command.call_args_list[0].args,
                ('skopeo', 'inspect', '--tls-verify=true', '--raw', 'docker://' + self.reference))
            self.assertEqual(command.call_args_list[1].args,
                ('skopeo', 'inspect', '--tls-verify=true', '--config', '--raw', 'docker://' + self.reference))
        restart_fixture(self)
        with self.subTest(phase="registry_configuration_mismatch_blocks_pull"):
            with patch.object(wpe, 'native_host'), \
                 patch.object(wpe, 'registry_identity', return_value='sha256:' + '9' * 64), \
                 patch.object(wpe, 'command') as commands:
                with self.assertRaises(ValueError):
                    wpe.import_stage(self.receipt, self.inputs)
                commands.assert_not_called()
        restart_fixture(self)
        with self.subTest(phase="failed_tool_diagnostics_are_not_exposed"):
            with patch.object(wpe.subprocess, 'check_output', side_effect=
                    subprocess.CalledProcessError(1, ['SECRET'], stderr=b'PRIVATE AUTH')) as command:
                with self.assertRaisesRegex(ValueError, '^external verification command failed$'):
                    wpe.command('tool', 'argument')
                self.assertEqual(command.call_args.kwargs['stderr'], subprocess.PIPE)

    def test_import_and_projection(self):
        with self.subTest(phase="import_only_never_builds_or_runs_and_checks_payload"):
            calls = []
            image = dict(Id=self.identity, Architecture='amd64', Os='linux', RepoDigests=[self.reference])
            def command(*args):
                calls.append(args)
                if args[:4] == ('podman', '--remote=false', 'image', 'inspect'):
                    return json.dumps([image]).encode()
                if args[:3] == ('podman', '--remote=false', 'pull'):
                    return b''
                self.fail('unexpected command: ' + repr(args))
            with patch.object(wpe, 'native_host'), patch.object(wpe, 'registry_identity', return_value=self.identity), \
                 patch.object(wpe, 'command', side_effect=command), \
                 patch.object(wpe, 'mounted', return_value=nullcontext(self.repo)), \
                 patch.object(wpe, 'payload_identity', return_value=self.receipt['payload']):
                self.assertEqual(wpe.import_stage(self.receipt, self.inputs), self.identity)
            self.assertEqual(len(calls), 2)
            with patch.object(wpe, 'command') as forbidden:
                with self.assertRaises(ValueError):
                    wpe.import_stage(self.receipt | {'dependency_key': 'wrong'}, self.inputs)
                forbidden.assert_not_called()
        restart_fixture(self)
        with self.subTest(phase="projection_copies_only_payload_without_running_image_code"):
            projected = 'sha256:' + '3' * 64
            def command(*args):
                self.assertEqual(args[:3], ('podman', '--remote=false', 'build'))
                self.assertIn('--pull=never', args)
                self.assertIn('--network=none', args)
                self.assertIn('compiled-wpe=container-image://' + self.identity, args)
                self.assertEqual(args[args.index('--source-date-epoch') + 1], '0')
                context = Path(args[-1])
                self.assertEqual((context / 'Containerfile').read_text(),
                                 'FROM scratch\nCOPY --from=compiled-wpe /wpe/ /wpe/\n')
                Path(args[args.index('--iidfile') + 1]).write_text(projected)
                return b''
            with patch.object(wpe, 'native_host'), patch.object(wpe, 'inspect_local'), \
                 patch.object(wpe, 'mounted', return_value=nullcontext(self.repo)), \
                 patch.object(wpe, 'payload_identity', return_value={'hash': 'original'}), \
                 patch.object(wpe, 'command', side_effect=command):
                self.assertEqual(wpe._project_payload(self.identity, self.inputs), projected)
            with patch.object(wpe, 'native_host'), patch.object(wpe, 'inspect_local'), \
                 patch.object(wpe, 'mounted', return_value=nullcontext(self.repo)), \
                 patch.object(wpe, 'payload_identity', side_effect=[{'hash': 'original'}, {'hash': 'changed'}]), \
                 patch.object(wpe, 'command', side_effect=command):
                with self.assertRaises(ValueError):
                    wpe._project_payload(self.identity, self.inputs)
        restart_fixture(self)
        with self.subTest(phase="bounded_regular_reads_and_strict_json"):
            path = self.repo / 'receipt.json'
            path.write_bytes(b'x' * 20)
            with self.assertRaises(ValueError):
                wpe.read_bounded(path, 10)
            link = self.repo / 'link'
            link.symlink_to(path)
            with self.assertRaises(OSError):
                wpe.read_bounded(link, 30)
            for value in ('{"schema":1,"schema":2}', '{"nested":{"x":1,"x":2}}', '{"x":NaN}'):
                with self.assertRaises(ValueError):
                    wpe.strict_json(value)
            script = self.repo / 'os/dev/build-wpe.sh'
            script.write_bytes(b'x' * (2 * 1024 * 1024 + 1))
            with self.assertRaises(ValueError):
                self.dependencies()

    def test_payload_export_checks(self):
        with self.subTest(phase="payload_rejects_missing"):
            root = self.repo / 'mount'
            with self.assertRaises(ValueError):
                wpe.payload_identity(root, 'x86_64')
        restart_fixture(self)
        with self.subTest(phase="producer_exports_only_id_from_same_build_and_rechecks_inputs"):
            args = SimpleNamespace(repo=self.repo, assets=self.assets, architecture='x86_64',
                epoch='0', wpe=self.buildroot, jobs=6, revision='a' * 40, run_id='42', attempt='1')
            with patch.object(wpe, 'verify_source'), \
                 patch.object(wpe, '_build_stage', return_value=self.identity) as build, \
                 patch.object(wpe, '_project_payload', return_value='sha256:' + '3' * 64) as project, \
                 patch.object(wpe, '_export_stage', return_value=self.receipt) as export:
                self.assertEqual(wpe.build_export(args), self.receipt)
                build.assert_called_once_with(args)
                project.assert_called_once_with(self.identity, self.inputs)
                export.assert_called_once_with('sha256:' + '3' * 64, self.inputs, 6, 'a' * 40, '42', '1',
                                               source_completed_stage_id=self.identity)
            def changed(_):
                (self.repo / 'os/Containerfile.wpe').write_text('changed while building')
                return self.identity
            with patch.object(wpe, 'verify_source'), patch.object(wpe, '_build_stage', side_effect=changed), \
                 patch.object(wpe, '_export_stage') as export:
                with self.assertRaises(ValueError):
                    wpe.build_export(args)
                export.assert_not_called()
        restart_fixture(self)
        with self.subTest(phase="export_rejects_incomplete_payload_before_publication"):
            with patch.object(wpe, 'native_host'), \
                 patch.object(wpe, 'inspect_local', return_value=self.identity), \
                 patch.object(wpe, 'mounted', return_value=nullcontext(self.repo)), \
                 patch.object(wpe, 'command') as commands:
                with self.assertRaises(ValueError):
                    wpe._export_stage(self.identity, self.inputs, 6, 'a' * 40, '42', '1')
                commands.assert_not_called()
        restart_fixture(self)
        with self.subTest(phase="payload_rejects_wrong_architecture_and_escaping_link"):
            root = self.repo / 'mount'
            payload = root / 'wpe'
            for name in ('usr/lib64/libWPEWebKit-2.0.so', 'usr/lib64/pkgconfig/wpe-webkit-2.0.pc',
                         'usr/libexec/wpe-webkit-2.0/WPEWebProcess'):
                path = payload / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b'pc' if name.endswith('.pc') else b'\x7fELF\x02\x01' + b'\0' * 12 + b'\x3e\0')
            self.assertEqual(len(wpe.payload_identity(root, 'x86_64')), 3)
            with self.assertRaises(ValueError):
                wpe.payload_identity(root, 'aarch64')
            library = payload / 'usr/lib64/libWPEWebKit-2.0.so'
            library.unlink()
            library.symlink_to(self.repo / 'os/Containerfile')
            with self.assertRaises(ValueError):
                wpe.payload_identity(root, 'x86_64')


if __name__ == '__main__':
    unittest.main()
