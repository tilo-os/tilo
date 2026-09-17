import argparse
from contextlib import ExitStack, nullcontext
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

DEV = Path(__file__).resolve().parents[1] / 'dev'
sys.path.insert(0, str(DEV))
SPEC = importlib.util.spec_from_file_location('rootfs_signing', DEV / 'sign-development-rootfs.py')
producer = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(producer)
IMAGE = 'sha256:' + 'a' * 64
REVISION = 'b' * 40


class RootfsIdentityTests(unittest.TestCase):
    def image(self):
        return {'Id': IMAGE, 'Os': 'linux', 'Architecture': 'arm64',
                'Labels': {'org.opencontainers.image.revision': REVISION}}

    def test_exact_local_identity_accepts_both_podman_id_formats(self):
        data = self.image()
        for value in (IMAGE, IMAGE.removeprefix('sha256:')):
            data['Id'] = value
            producer.identity(json.dumps([data]), IMAGE, 'aarch64', REVISION)

    def test_wrong_identity_platform_or_revision_rejected(self):
        for field, value in [('Id', 'sha256:' + 'c' * 64), ('Architecture', 'amd64'),
                             ('Os', 'windows'), ('Labels', {}), ('Labels', None)]:
            data = self.image()
            data[field] = value
            with self.subTest(field=field, value=value), self.assertRaises(ValueError):
                producer.identity(json.dumps([data]), IMAGE, 'aarch64', REVISION)
        for images in ([], [self.image(), self.image()]):
            with self.assertRaises(ValueError):
                producer.identity(json.dumps(images), IMAGE, 'aarch64', REVISION)

    def test_duplicate_inspection_keys_rejected(self):
        with self.assertRaises(ValueError):
            producer.identity('[{"Id":"first","Id":"second"}]', IMAGE, 'aarch64', REVISION)

    def arguments(self):
        return argparse.Namespace(rootfs_id=IMAGE, architecture='aarch64', revision=REVISION,
                                  target='code.podesta.ai/qos/tilo-development:testing-aarch64',
                                  signer=Path('/var/lib/tilo-test-identity/signer'))

    def test_preflight_rejects_mutable_ids_non_native_inputs_and_remote_connections(self):
        with ExitStack() as stack:
            stack.enter_context(patch.object(producer.platform, 'machine', return_value='aarch64'))
            stack.enter_context(patch.object(producer.os, 'getuid', return_value=0, create=True))
            stack.enter_context(patch.object(producer.signing, 'trusted', side_effect=lambda path: path))
            stack.enter_context(patch.object(producer.os, 'access', return_value=True))
            stack.enter_context(patch.dict(os.environ, {}, clear=True))
            producer.inputs(self.arguments())
            for field, value in [('rootfs_id', 'localhost/image:latest'), ('rootfs_id', 'a' * 12),
                                 ('architecture', 'x86_64'), ('revision', 'main'),
                                 ('target', 'code.podesta.ai/qos/tilo:testing-aarch64'),
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

    def test_failed_mounted_validation_always_unmounts_exact_id(self):
        mount_path = str(DEV.resolve()) + '\n'
        with patch.object(producer, 'command', side_effect=[mount_path, '']) as command:
            with self.assertRaisesRegex(ValueError, 'bad embedded trust'):
                with producer.mounted(IMAGE):
                    raise ValueError('bad embedded trust')
            self.assertEqual(command.call_args_list[-1].args[0], ['podman', 'image', 'unmount', IMAGE])

    def test_bad_embedded_trust_stops_before_signer_or_build_execution(self):
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
