import importlib.util
from pathlib import Path
import subprocess
import tempfile
import shutil
import unittest

MODULE = Path(__file__).resolve().parents[1] / 'dev/remote_source_prepare.py'
SPEC = importlib.util.spec_from_file_location('remote_source_prepare', MODULE)
BUILD = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BUILD)


class SourceBoundaryTests(unittest.TestCase):
    def fixture(self, root):
        source, mirror, receipt = root / "owner", root / "mirror", root / "receipt.json"
        source.mkdir()
        subprocess.run(["git", "init", "-q", str(source)], check=True)
        for name in ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "apps/example/src/main.rs"):
            path = source / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("fixture")
        subprocess.run(["git", "-C", str(source), "add", "."], check=True)
        return source, mirror, receipt

    def test_trusted_parent_alias_does_not_look_like_a_symlink_inside_the_mirror(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            source, mirror, receipt = self.fixture(root)
            alias = root / "alias"
            alias.symlink_to(root, target_is_directory=True)
            result = BUILD.prepare(source, alias / mirror.name, receipt)
            self.assertTrue(result["source_only"])
            self.assertEqual((mirror / "Cargo.toml").read_text(), "fixture")

    def test_receipt_cannot_overwrite_source_or_enter_mirror(self):
        with tempfile.TemporaryDirectory() as directory:
            source, mirror, receipt = self.fixture(Path(directory))
            for target in (source / "Cargo.toml", mirror / "receipt.json"):
                with self.subTest(target=target):
                    with self.assertRaisesRegex(ValueError, "outside"):
                        BUILD.prepare(source, mirror, target)
            self.assertEqual((source / "Cargo.toml").read_text(), "fixture")
            self.assertFalse(mirror.exists())
            result = subprocess.run(["python3", str(MODULE), "--source", str(source),
                                     "--mirror", str(mirror), "--receipt", str(source / "new/receipt.json")],
                                    capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse((source / "new").exists())

    def test_failed_refresh_invalidates_previous_success_receipt(self):
        with tempfile.TemporaryDirectory() as directory:
            source, mirror, receipt = self.fixture(Path(directory))
            BUILD.prepare(source, mirror, receipt)
            self.assertTrue(receipt.is_file())
            (source / "Cargo.toml").write_text("new snapshot")
            (mirror / "unexpected").touch()
            with self.assertRaisesRegex(ValueError, "unexpected upload"):
                BUILD.prepare(source, mirror, receipt)
            self.assertEqual((mirror / "Cargo.toml").read_text(), "new snapshot")
            self.assertFalse(receipt.exists(), "old success must not attest the modified mirror")
            (mirror / "unexpected").unlink()
            BUILD.prepare(source, mirror, receipt)
            self.assertTrue(receipt.is_file())

    def test_stale_source_pruning_cannot_follow_mirror_symlink(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, mirror, receipt = self.fixture(root)
            BUILD.prepare(source, mirror, receipt)
            old = source / "apps/example/src/main.rs"
            old.unlink()
            subprocess.run(["git", "-C", str(source), "rm", "-q", "apps/example/src/main.rs"], check=True)
            # No current input visits apps/, so only stale-path pruning sees it.
            shutil.rmtree(mirror / "apps")
            external = root / "external"
            victim = external / "example/src/main.rs"
            victim.parent.mkdir(parents=True)
            victim.write_text("unrelated source")
            (mirror / "apps").symlink_to(external, target_is_directory=True)
            with self.assertRaisesRegex(ValueError, "symlink"):
                BUILD.prepare(source, mirror, receipt)
            self.assertEqual(victim.read_text(), "unrelated source")

    def test_mirror_metadata_symlink_cannot_overwrite_external_file(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, mirror, receipt = self.fixture(root)
            BUILD.prepare(source, mirror, receipt)
            victim = root / "external.json"
            victim.write_text("{}")
            (mirror / ".source-manifest.json").unlink()
            (mirror / ".source-manifest.json").symlink_to(victim)
            with self.assertRaisesRegex(ValueError, "symlink"):
                BUILD.prepare(source, mirror, receipt)
            self.assertEqual(victim.read_text(), "{}")

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
