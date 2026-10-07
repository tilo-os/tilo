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
    'browser-runtime-input': 'crates/browser/runtime-input',
    'browser-runtime-leaf': 'crates/browser/runtime-leaf',
}


class AppSdkAffectedTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.metadata = {'packages': [
            package(self.root, 'crates/ui', 'ui'),
            package(self.root, 'crates/session/shell', 'shell', ('ui',)),
            package(self.root, 'crates/dev/tilo-app-cli', 'tilo-app-cli'),
            # The CLI builds this through a subprocess, not a Cargo dependency.
            package(self.root, 'crates/browser/tilo-browser-compute', 'tilo-browser-compute'),
            # Only the runtime reaches this chain; no app depends on it.
            package(self.root, 'crates/browser/runtime-leaf', 'browser-runtime-leaf'),
            package(self.root, 'crates/browser/runtime-input', 'browser-runtime-input',
                    ('browser-runtime-leaf',)),
            package(self.root, 'crates/browser/tilo-browser-runtime', 'tilo-browser-runtime',
                    ('browser-runtime-input',)),
            package(self.root, 'crates/browser/tilo-web-desktop', 'tilo-web-desktop'),
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

    def test_private_compute_without_cargo_dependents_runs_the_lanes(self):
        for path in ('crates/browser/tilo-browser-compute/src/lib.rs',
                     'crates/browser/tilo-browser-compute/web/compute-worker.mjs'):
            with self.subTest(path=path):
                self.assertTrue(self.check(path))

    def test_browser_runtime_code_and_font_loader_assets_run_the_lanes(self):
        for path in ('crates/browser/tilo-browser-runtime/src/browser.rs',
                     'crates/browser/tilo-browser-runtime/web/font-loader.mjs'):
            with self.subTest(path=path):
                self.assertTrue(self.check(path))

    def test_desktop_bundle_and_its_smoke_script_run_the_lanes(self):
        for path in ('crates/browser/tilo-web-desktop/src/lib.rs',
                     'crates/browser/tilo-web-desktop/scripts/smoke.mjs'):
            with self.subTest(path=path):
                self.assertTrue(self.check(path))

    def test_runtime_only_transitive_dependencies_run_the_lanes(self):
        self.assertTrue(self.check('crates/browser/runtime-input/src/lib.rs'))
        self.assertTrue(self.check('crates/browser/runtime-leaf/src/lib.rs'))

    def test_workflow_events_watch_the_selector_and_graph(self):
        workflow = (ROOT / '.forgejo/workflows/app-sdk.yml').read_text()
        for event, following in (('pull_request', 'push'), ('push', 'workflow_dispatch')):
            section = workflow.split(f'  {event}:\n', 1)[1].split(f'  {following}:', 1)[0]
            for path in ('tools/ci/app_sdk_affected.py', 'tools/ci/workspace_graph.py'):
                with self.subTest(event=event, path=path):
                    self.assertIn(f'      - "{path}"', section)

    def test_a_crate_no_app_reaches_skips_the_lanes(self):
        self.assertFalse(self.check('crates/session/shell/src/lib.rs'))
        self.assertFalse(self.check('os/Containerfile', 'docs/ci.md'))

    def test_repository_wide_inputs_run_the_lanes(self):
        self.assertTrue(self.check('rust-toolchain.toml'))
        self.assertTrue(self.check('.forgejo/workflows/app-sdk.yml'))
        self.assertTrue(self.check('crates/stray-file.rs'))
        self.assertTrue(self.check('tools/ci/app_sdk_affected.py'))
        self.assertTrue(self.check('tools/ci/workspace_graph.py'))


if __name__ == '__main__':
    unittest.main()
