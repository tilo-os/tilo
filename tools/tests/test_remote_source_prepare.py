import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest

MODULE = Path(__file__).resolve().parents[1] / 'dev/remote_source_prepare.py'
SPEC = importlib.util.spec_from_file_location('remote_source_prepare', MODULE)
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
        self.assertTrue(BUILD.allowed('tools/cargo-runner.sh', True))
        self.assertTrue(BUILD.allowed('tools/with-test-environment.sh', True))
        self.assertTrue(BUILD.allowed('tools/font-catalog.py', True))
        self.assertFalse(BUILD.allowed('tools/unreviewed-upload.sh', True))
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

    def test_public_compile_includes_are_mirrored_without_broadening_data_access(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, mirror = root / 'owner', root / 'mirror'
            source.mkdir()
            subprocess.run(['git', 'init', '-q', str(source)], check=True)
            public = sorted(BUILD.SOURCE_HELPERS)
            private = ['os/private.env', 'docs/customer-data.json',
                       'tools/protocols/credentials.json', 'dist/private.env']
            for name in [*BUILD.ROOT_FILES, *public, *private]:
                path = source / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text('fixture:' + name)
            subprocess.run(['git', '-C', str(source), 'add', '.'], check=True)
            BUILD.prepare(source, mirror, root / 'receipt.json')
            for name in public:
                self.assertEqual((mirror / name).read_bytes(), (source / name).read_bytes(), name)
            for name in private:
                self.assertFalse((mirror / name).exists(), name)
            self.assertFalse(BUILD.allowed('docs/architecture/dictation-whistle-aot-image.json', False))

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
