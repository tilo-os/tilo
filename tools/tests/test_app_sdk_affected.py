import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'tools'))
SPEC = importlib.util.spec_from_file_location('app_sdk_affected', ROOT / 'tools/ci/app_sdk_affected.py')
selector = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(selector)


def package(root: Path, rel: str, name: str, deps: tuple[str, ...] = ()) -> dict:
    manifest = root / rel / 'Cargo.toml'
    manifest.parent.mkdir(parents=True, exist_ok=True)
    manifest.write_text(f'[package]\nname = "{name}"\n')
    return {
        'name': name,
        'manifest_path': str(manifest),
        'dependencies': [{'name': d, 'path': str(root / dep_dirs[d])} for d in deps],
    }


dep_dirs = {
    'ui': 'crates/ui',
    'shell': 'crates/session/shell',
    'tilo-app-cli': 'crates/dev/tilo-app-cli',
}


class AppSdkAffectedTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.metadata = {'packages': [
            package(self.root, 'crates/ui', 'ui'),
            package(self.root, 'crates/session/shell', 'shell', ('ui',)),
            package(self.root, 'crates/dev/tilo-app-cli', 'tilo-app-cli'),
            package(self.root, 'apps/calculator', 'calculator', ('ui',)),
        ]}

    def tearDown(self):
        self.tmp.cleanup()

    def check(self, *paths: str) -> bool:
        return selector.affected(self.root, list(paths), self.metadata, 'base', 'head')[0]

    def test_a_crate_an_app_depends_on_runs_the_lanes(self):
        self.assertTrue(self.check('crates/ui/src/lib.rs'))

    def test_an_app_or_the_sdk_runs_the_lanes(self):
        self.assertTrue(self.check('apps/calculator/src/main.rs'))
        self.assertTrue(self.check('crates/dev/tilo-app-cli/src/main.rs'))

    def test_a_crate_no_app_reaches_skips_the_lanes(self):
        self.assertFalse(self.check('crates/session/shell/src/lib.rs'))
        self.assertFalse(self.check('os/Containerfile', 'docs/ci.md'))

    def test_repository_wide_inputs_run_the_lanes(self):
        self.assertTrue(self.check('rust-toolchain.toml'))
        self.assertTrue(self.check('.forgejo/workflows/app-sdk.yml'))
        self.assertTrue(self.check('crates/stray-file.rs'))


if __name__ == '__main__':
    unittest.main()
