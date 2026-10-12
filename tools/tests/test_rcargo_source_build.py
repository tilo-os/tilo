import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest
import io
import json
import os
from unittest import mock

MODULE = Path(__file__).resolve().parents[1] / 'dev/rcargo_source_build.py'
SPEC = importlib.util.spec_from_file_location('rcargo_source_build', MODULE)
BUILD = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BUILD)


class SourceBoundaryTests(unittest.TestCase):
    def test_payloads_secrets_and_credentials_never_enter_allowlist(self):
        for name in ['apps/good-morning/.env', 'apps/good-morning/.env.local', 'crates/a/.aws/config',
                     'apps/good-morning/imports/mail.json', 'crates/a/credentials/config',
                     'crates/a/private.pem', 'apps/good-morning/mail.jsonl', 'target/debug/tool',
                     '/etc/passwd', '../Cargo.toml', 'keys/public.json']:
            self.assertFalse(BUILD.allowed(name, True), name)
        self.assertTrue(BUILD.allowed('crates/text/emoji/data/en.txt', True))
        self.assertTrue(BUILD.allowed('apps/good-morning/src/main.rs', False))
        self.assertFalse(BUILD.allowed('apps/good-morning/untracked-data.json', False))

    def test_production_preparation_only_mirrors_source_and_refuses_extra_files(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, mirror = root / 'owner', root / 'mirror'
            source.mkdir()
            subprocess.run(['git', 'init', '-q', str(source)], check=True)
            for name in ['Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', 'apps/good-morning/src/main.rs',
                         'apps/good-morning/.env', 'apps/good-morning/imports/mail.json']:
                path = source / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text('fixture')
            subprocess.run(['git', '-C', str(source), 'add', 'Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml'], check=True)
            receipt = root / 'receipt.json'
            result = BUILD.prepare(source, mirror, receipt)
            self.assertTrue(result['source_only'])
            self.assertFalse(result['build_executed'])
            self.assertFalse((mirror / 'apps/good-morning/.env').exists())
            self.assertFalse((mirror / 'apps/good-morning/imports/mail.json').exists())
            (mirror / 'unexpected.txt').write_text('do not upload')
            with self.assertRaisesRegex(ValueError, 'unexpected upload'):
                BUILD.prepare(source, mirror, receipt)

    def test_build_forces_ssh_no_fallback_and_omits_aws_recovery(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            config = root / 'config.toml'
            config.write_text('[builder]\nhost="existing.example"\nuser="fedora"\ninstance_id="must-not-forward"\nsecurity_group_id="must-not-forward"\n')
            receipt = root / 'result.json'
            result = {'command': ['rcargo', '--no-fallback', 'build', '--locked', '-p', 'good-morning', '--bins']}
            process = mock.Mock(stdout=io.BytesIO(b'rcargo: timing server.command 123.0ms\n'), returncode=101)
            image = 'registry.tilo.so/tilo/tilo-builder@sha256:' + 'a' * 64
            with mock.patch.dict(os.environ, {'RCARGO_IMAGE': image, 'RCARGO_TRANSPORT': 'local', 'CARGO_TARGET_DIR': '/forbidden'}):
                with mock.patch.object(BUILD.subprocess, 'Popen', return_value=process) as spawn:
                    self.assertEqual(BUILD.build(root, receipt, result, config), 101)
            env = spawn.call_args.kwargs['env']
            self.assertEqual(env['RCARGO_TRANSPORT'], 'ssh')
            self.assertEqual(env['RCARGO_FALLBACK'], '0')
            self.assertNotIn('CARGO_TARGET_DIR', env)
            scoped = (root / 'host-only-rcargo.toml').read_text()
            self.assertNotIn('instance_id', scoped)
            self.assertNotIn('security_group_id', scoped)
            self.assertEqual(json.loads(receipt.read_text())['phases_seconds']['server.command'], .123)

    def test_symlink_source_cannot_read_external_file(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory)
            subprocess.run(['git', 'init', '-q', str(source)], check=True)
            for name in ['Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml']:
                (source / name).write_text('fixture')
            path = source / 'apps/good-morning/src/main.rs'
            path.parent.mkdir(parents=True)
            path.symlink_to('/etc/passwd')
            with self.assertRaisesRegex(ValueError, 'regular source'):
                BUILD.source_files(source)


if __name__ == '__main__':
    unittest.main()
