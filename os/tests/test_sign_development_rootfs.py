import argparse
from contextlib import ExitStack, nullcontext
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

DEV = Path(__file__).resolve().parents[1] / 'dev'
sys.path.insert(0, str(DEV))
SPEC = importlib.util.spec_from_file_location('rootfs_signing', DEV / 'sign-development-rootfs.py')
producer = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(producer)
FIXTURE_SPEC = importlib.util.spec_from_file_location(
    'development_rootfs_fixture', Path(__file__).with_name('development_rootfs.py'))
fixture = importlib.util.module_from_spec(FIXTURE_SPEC)
FIXTURE_SPEC.loader.exec_module(fixture)
IMAGE = 'sha256:' + 'a' * 64
REVISION = 'b' * 40


class RootfsIdentityTests(unittest.TestCase):
    def test_signing_environment(self):
        with self.subTest(phase="loader_layer_uses_verified_hashes_without_target_crypto_tools"):
            containerfile = (DEV.parent / 'Containerfile.development-loader').read_text()
            self.assertIn('ARG ORIGINAL_SHA256', containerfile)
            self.assertIn('ARG SIGNED_SHA256', containerfile)
            self.assertIn('verify-authenticode-envelope.py', containerfile)
            self.assertNotIn('/usr/bin/openssl', containerfile)
            self.assertNotIn('verify-development-loader.py', containerfile)
        with self.subTest(phase="podman_preserves_explicit_local_storage_without_caller_secrets"):
            settings = {name: '/fixture/' + name.lower() for name in producer.LOCAL_STORAGE_ENVIRONMENT}
            settings.update({'PATH': '/untrusted/bin', 'UNRELATED_SECRET': 'not-for-child',
                             'CONTAINER_HOST': 'ssh://remote', 'REGISTRY_AUTH_FILE': '/private/auth'})
            with patch.dict(os.environ, settings, clear=True), patch.object(
                    producer.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, b'checked', b'')) as run:
                self.assertEqual(producer.command(['podman', 'image', 'inspect', IMAGE]), 'checked')
            self.assertEqual(run.call_args.args[0], ['/usr/bin/podman', '--remote=false', 'image', 'inspect', IMAGE])
            environment = run.call_args.kwargs['env']
            self.assertEqual(environment, {'PATH': '/usr/bin:/bin', 'LANG': 'C.UTF-8',
                                          **{name: settings[name] for name in producer.LOCAL_STORAGE_ENVIRONMENT}})
        with self.subTest(phase="signer_receives_clean_environment_without_storage_configuration"):
            with patch.dict(os.environ, {'HOME': '/fixture/home', 'CONTAINERS_STORAGE_CONF': '/fixture/storage.conf',
                                         'UNRELATED_SECRET': 'not-for-signer'}, clear=True), patch.object(
                    producer.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, b'', b'')) as run:
                producer.command(['/var/lib/identity/signer', 'sign-efi', '/input', '/output'])
            self.assertEqual(run.call_args.kwargs['env'],
                             {'PATH': '/usr/bin:/bin', 'LANG': 'C.UTF-8', 'HOME': '/root'})
        with self.subTest(phase="failed_command_reports_bounded_stderr"):
            stderr = ('discarded-prefix-' + 'x' * 4096).encode()
            with patch.object(producer.subprocess, 'run', return_value=subprocess.CompletedProcess(
                    [], 125, b'', stderr)), self.assertRaisesRegex(
                        ValueError, r'^podman failed \(exit 125\): x{4096}$'):
                producer.command(['podman', 'image', 'inspect', IMAGE])

    def image(self):
        return {'Id': IMAGE, 'Os': 'linux', 'Architecture': 'arm64',
                'Labels': {'org.opencontainers.image.revision': REVISION}}

    def test_local_identity_checks(self):
        with self.subTest(phase="exact_local_identity_accepts_both_podman_id_formats"):
            data = self.image()
            for value in (IMAGE, IMAGE.removeprefix('sha256:')):
                data['Id'] = value
                producer.identity(json.dumps([data]), IMAGE, 'aarch64', REVISION)
        with self.subTest(phase="wrong_identity_platform_or_revision_rejected"):
            for field, value in [('Id', 'sha256:' + 'c' * 64), ('Architecture', 'amd64'),
                                 ('Os', 'windows'), ('Labels', {}), ('Labels', None)]:
                data = self.image()
                data[field] = value
                with self.subTest(field=field, value=value), self.assertRaises(ValueError):
                    producer.identity(json.dumps([data]), IMAGE, 'aarch64', REVISION)
            for images in ([], [self.image(), self.image()]):
                with self.assertRaises(ValueError):
                    producer.identity(json.dumps(images), IMAGE, 'aarch64', REVISION)
        with self.subTest(phase="duplicate_inspection_keys_rejected"):
            with self.assertRaises(ValueError):
                producer.identity('[{"Id":"first","Id":"second"}]', IMAGE, 'aarch64', REVISION)
        with self.subTest(phase="preflight_rejects_mutable_ids_non_native_inputs_and_remote_connections"):
            with ExitStack() as stack:
                stack.enter_context(patch.object(producer.platform, 'machine', return_value='aarch64'))
                stack.enter_context(patch.object(producer.os, 'getuid', return_value=0, create=True))
                stack.enter_context(patch.object(producer.signing, 'trusted', side_effect=lambda path: path))
                stack.enter_context(patch.object(producer.os, 'access', return_value=True))
                stack.enter_context(patch.dict(os.environ, {}, clear=True))
                producer.inputs(self.arguments())
                for field, value in [('rootfs_id', 'localhost/image:latest'), ('rootfs_id', 'a' * 12),
                                     ('architecture', 'x86_64'), ('revision', 'main'),
                                     *[('source_date_epoch', value) for value in
                                       ('', '-1', '1.5', '01', ' 1', '253402300800')],
                                     ('target', 'registry.tilo.so/tilo/tilo:testing-aarch64'),
                                     ('signer', Path('/var/lib/../signer')),
                                     ('signer', DEV / 'development-signer.py')]:
                    args = self.arguments()
                    setattr(args, field, value)
                    with self.subTest(field=field), self.assertRaises(ValueError):
                        producer.inputs(args)
                for name in ('CONTAINER_HOST', 'CONTAINER_CONNECTION', 'CONTAINER_SSHKEY', 'DOCKER_HOST'):
                    with patch.dict(os.environ, {name: 'unexpected-remote'}), self.assertRaises(ValueError):
                        producer.inputs(self.arguments())
                for name in ('RUNNER_TEMP', 'GITHUB_WORKSPACE'):
                    with patch.dict(os.environ, {name: '/var/lib'}), self.assertRaises(ValueError):
                        producer.inputs(self.arguments())

    def arguments(self):
        return argparse.Namespace(rootfs_id=IMAGE, architecture='aarch64', revision=REVISION,
                                  source_date_epoch='1700000000',
                                  target='registry.tilo.so/tilo/tilo-development:testing-aarch64',
                                  signer=Path('/var/lib/tilo-test-identity/signer'))

    def test_mounted_validation(self):
        with self.subTest(phase="failed_mounted_validation_always_unmounts_exact_id"):
            mount_path = str(DEV.resolve()) + '\n'
            with patch.object(producer, 'command', side_effect=[mount_path, '']) as command:
                with self.assertRaisesRegex(ValueError, 'bad embedded trust'):
                    with producer.mounted(IMAGE):
                        raise ValueError('bad embedded trust')
                self.assertEqual(command.call_args_list[-1].args[0], ['podman', 'image', 'unmount', IMAGE])
        with self.subTest(phase="bad_embedded_trust_stops_before_signer_or_build_execution"):
            with tempfile.TemporaryDirectory() as temporary, ExitStack() as stack:
                root = Path(temporary)
                public = root / 'input-public'
                public.mkdir()
                work = root / 'private-work'
                work.mkdir()
                args = self.arguments()
                args.public_bundle = public
                args.manifest_sha256 = 'c' * 64
                stack.enter_context(patch.object(producer, 'inputs'))
                stack.enter_context(patch.object(producer.tempfile, 'TemporaryDirectory', return_value=nullcontext(str(work))))
                stack.enter_context(patch.object(producer.trust, 'validate', return_value={
                    'identities': {'efi-signing.crt': 'd' * 64}}))
                stack.enter_context(patch.object(producer, 'mounted', return_value=nullcontext(root)))
                stack.enter_context(patch.object(producer.source, 'mounted_identity', side_effect=ValueError('bad trust')))
                command = stack.enter_context(patch.object(producer, 'command', return_value=json.dumps([self.image()])))
                with self.assertRaisesRegex(ValueError, 'bad trust'):
                    producer.produce(args)
                self.assertEqual(command.call_args_list[0].args[0], ['podman', 'image', 'inspect', IMAGE])
                self.assertEqual(command.call_count, 1)


if __name__ == '__main__':
    unittest.main()
