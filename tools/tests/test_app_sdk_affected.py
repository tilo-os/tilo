import importlib.util
from pathlib import Path
import re
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

    def linux(self, *paths: str) -> bool:
        return selector.affected(self.root, list(paths), self.metadata,
                                 'base', 'head', lane='linux')[0]

    def other_app(self):
        # Match observed Cargo metadata: every native consumer has a source target.
        for member in self.metadata['packages']:
            source = Path(member['manifest_path']).parent / 'src/lib.rs'
            source.parent.mkdir(parents=True, exist_ok=True)
            source.write_text('pub fn fixture() {}\n')
            member['targets'] = [{'kind': ['lib'], 'src_path': str(source)}]
        app = package(self.root, 'apps/other', 'other', ('ui',))
        source = self.root / 'apps/other/src/main.rs'
        source.parent.mkdir()
        source.write_text('fn main() {}\n')
        self.metadata['packages'].append(app)
        return app, source

    def test_other_app_source_keeps_web_and_native_app_tests_not_linux_sdk(self):
        self.other_app()
        self.assertTrue(self.check('apps/other/src/main.rs'))
        self.assertFalse(self.linux('apps/other/src/main.rs'))
        self.assertTrue(self.linux('apps/calculator/src/main.rs'))
        self.assertTrue(self.linux('apps/other/Cargo.toml'))
        self.assertTrue(self.linux('apps/other/locales/en-US.ftl'))
        self.assertTrue(self.linux('apps/other/src/main.rs', 'crates/ui/src/lib.rs'))

    def test_runtime_subprocess_and_dependencies_keep_both_lanes(self):
        runtime = package(self.root, 'crates/runtime/runtime-dylib', 'runtime-dylib', ('ui',))
        self.metadata['packages'].append(runtime)
        for path in ('crates/runtime/runtime-dylib/src/lib.rs', 'crates/ui/src/lib.rs'):
            self.assertTrue(self.check(path))
            self.assertTrue(self.linux(path))

    def test_app_consumed_by_a_native_root_keeps_linux_sdk(self):
        app, _ = self.other_app()
        sdk = next(p for p in self.metadata['packages'] if p['name'] == 'tilo-app-cli')
        sdk['dependencies'].append({'name': app['name'], 'path': str(self.root / 'apps/other')})
        self.assertTrue(self.linux('apps/other/src/main.rs'))

    def test_native_source_reference_and_alias_keep_linux_sdk(self):
        _, source = self.other_app()
        sdk = self.root / 'crates/dev/tilo-app-cli/src/lib.rs'
        sdk.parent.mkdir(exist_ok=True)
        sdk.write_text('include!("../../../../apps/other/src/main.rs");\n')
        self.assertTrue(self.linux('apps/other/src/main.rs'))
        sdk.write_text('pub fn sdk() {}\n')
        self.assertFalse(self.linux('apps/other/src/main.rs'))
        alias = sdk.parent / 'app'
        alias.symlink_to(source.parent, target_is_directory=True)
        self.assertTrue(self.linux('apps/other/src/main.rs'))

    def test_unobserved_native_consumer_keeps_linux_sdk(self):
        self.other_app()
        sdk = next(p for p in self.metadata['packages'] if p['name'] == 'tilo-app-cli')
        source = Path(sdk['targets'][0]['src_path'])
        source.unlink()
        self.assertTrue(self.linux('apps/other/src/main.rs'))

    def test_deleted_unknown_and_symlinked_app_sources_keep_linux_sdk(self):
        _, source = self.other_app()
        self.assertTrue(self.linux('apps/other/src/unknown.rs'))
        source.unlink()
        self.assertTrue(self.linux('apps/other/src/main.rs'))
        target = source.parent / 'real.rs'
        target.write_text('fn main() {}\n')
        source.symlink_to(target)
        self.assertTrue(self.linux('apps/other/src/main.rs'))

    def test_missing_calculator_observation_and_global_inputs_keep_linux_sdk(self):
        self.other_app()
        self.assertTrue(self.linux('apps/other/src/main.rs', 'tools/ci/app_sdk_affected.py'))
        self.metadata['packages'] = [p for p in self.metadata['packages'] if p['name'] != 'calculator']
        self.assertTrue(self.linux('apps/other/src/main.rs'))

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
            for path in ('tools/ci/app_sdk_affected.py', 'tools/ci/workspace_graph.py',
                         'tools/ci/sdk_tool_artifact.py'):
                with self.subTest(event=event, path=path):
                    self.assertIn(f'      - "{path}"', section)

    def test_a_crate_no_app_reaches_skips_the_lanes(self):
        self.assertFalse(self.check('crates/session/shell/src/lib.rs'))
        self.assertFalse(self.check('os/Containerfile', 'docs/ci.md'))

    def integration_fixture(self):
        shell = next(p for p in self.metadata['packages'] if p['name'] == 'shell')
        test_root = self.root / 'crates/session/shell/tests/it.rs'
        test_root.parent.mkdir()
        test_root.write_text('#[path = "peer_sync_test.rs"]\nmod peer_sync_test;\n')
        (test_root.parent / 'peer_sync_test.rs').write_text('#[test]\nfn peer_sync() {}\n')
        source = self.root / 'crates/session/shell/src/lib.rs'
        source.parent.mkdir()
        source.write_text('pub fn production() {}\n')
        shell['targets'] = [
            {'kind': ['test'], 'src_path': str(test_root)},
            {'kind': ['lib'], 'src_path': str(source)},
        ]
        calculator = next(p for p in self.metadata['packages'] if p['name'] == 'calculator')
        calculator['dependencies'].append({'name': 'shell',
                                          'path': str(source.parent.parent)})
        return shell, test_root, source

    def test_only_audited_integration_module_avoids_sdk_rebuild(self):
        self.integration_fixture()
        self.assertFalse(self.check('crates/session/shell/tests/peer_sync_test.rs'))
        self.assertTrue(self.check('crates/session/shell/src/lib.rs'))
        self.assertTrue(self.check('crates/session/shell/tests/it.rs'))
        self.assertTrue(self.check('crates/session/shell/tests/unknown.rs'))
        self.assertTrue(self.check('crates/session/shell/tests/fixtures/network-boundary.rs'))
        self.assertTrue(self.check('crates/session/shell/tests/peer_sync_test.rs',
                                   'crates/ui/src/lib.rs'))

    def test_source_reference_and_missing_observation_keep_sdk_selected(self):
        _, test_root, source = self.integration_fixture()
        source.write_text('#[path = "../tests/peer_sync_test.rs"]\nmod imported;\n')
        self.assertTrue(self.check('crates/session/shell/tests/peer_sync_test.rs'))
        source.unlink()
        self.assertTrue(self.check('crates/session/shell/tests/peer_sync_test.rs'))
        test_root.unlink()
        self.assertTrue(self.check('crates/session/shell/tests/peer_sync_test.rs'))

    def test_standalone_binary_and_build_script_keep_sdk_selected(self):
        shell, test_root, _ = self.integration_fixture()
        module = test_root.parent / 'peer_sync_test.rs'
        module.write_text('fn main() {}\n')
        shell['targets'].append({'kind': ['bin'], 'src_path': str(module)})
        self.assertTrue(self.check('crates/session/shell/tests/peer_sync_test.rs'))
        shell['targets'].pop()
        script = test_root.parent.parent / 'build.rs'
        script.write_text('fn main() {}\n')
        shell['targets'].append({'kind': ['custom-build'], 'src_path': str(script)})
        self.assertTrue(self.check('crates/session/shell/tests/peer_sync_test.rs'))

    def test_other_workspace_source_can_make_module_an_sdk_input(self):
        self.integration_fixture()
        consumer = self.root / 'apps/calculator/src/main.rs'
        consumer.parent.mkdir()
        consumer.write_text('const FIXTURE: &str = include_str!("peer_sync_test.rs");\n')
        self.assertTrue(self.check('crates/session/shell/tests/peer_sync_test.rs'))

    def standalone_fixture(self, name='calculator'):
        pkg = next(p for p in self.metadata['packages'] if p['name'] == name)
        path = Path(pkg['manifest_path']).parent / 'tests/native_flow.rs'
        path.parent.mkdir()
        path.write_text('#[test]\nfn native_flow() {}\n')
        source = path.parent.parent / 'src/main.rs'
        source.parent.mkdir()
        source.write_text('fn main() {}\n')
        pkg['targets'] = [{'kind': ['test'], 'src_path': str(path)},
                          {'kind': ['bin'], 'src_path': str(source)}]
        return pkg, path, source

    def test_standalone_app_test_does_not_rebuild_sdk_but_app_source_does(self):
        _, path, source = self.standalone_fixture()
        self.assertFalse(self.check(str(path.relative_to(self.root))))
        self.assertTrue(self.check(str(source.relative_to(self.root))))
        self.assertTrue(self.check(str(path.relative_to(self.root)),
                                   'apps/calculator/src/main.rs'))
        path.unlink()
        self.assertTrue(self.check(str(path.relative_to(self.root))))

    def test_sdk_owned_integration_targets_remain_inputs(self):
        _, path, _ = self.standalone_fixture('tilo-app-cli')
        self.assertTrue(self.check(str(path.relative_to(self.root))))
        caps = package(self.root, 'crates/caps', 'tilo-caps')
        self.metadata['packages'].append(caps)
        _, path, _ = self.standalone_fixture('tilo-caps')
        self.assertTrue(self.check(str(path.relative_to(self.root))))

    def test_app_test_reused_by_sdk_test_remains_an_input(self):
        _, path, _ = self.standalone_fixture()
        sdk_test = self.root / 'crates/dev/tilo-app-cli/tests/it.rs'
        sdk_test.parent.mkdir()
        sdk_test.write_text('const FIXTURE: &str = include_str!("native_flow.rs");\n')
        self.assertTrue(self.check(str(path.relative_to(self.root))))

    def test_app_test_binary_build_script_and_nested_inputs_fail_closed(self):
        pkg, path, _ = self.standalone_fixture()
        relative = str(path.relative_to(self.root))
        pkg['targets'].append({'kind': ['bin'], 'src_path': str(path)})
        self.assertTrue(self.check(relative))
        pkg['targets'].pop()
        pkg['targets'].append({'kind': ['custom-build'],
                               'src_path': str(path.parent.parent / 'build.rs')})
        self.assertTrue(self.check(relative))
        pkg['targets'].pop()
        self.assertTrue(self.check('apps/calculator/tests/fixtures/data.rs'))
        self.assertTrue(self.check('apps/calculator/tests/unknown.rs'))
        self.assertTrue(self.check('apps/calculator/tests/it.rs'))

    def test_sdk_workflow_tests_stay_inside_exempt_packages(self):
        workflow = (ROOT / '.forgejo/workflows/app-sdk.yml').read_text()
        for command in re.findall(r'cargo test[^\n]+', workflow):
            names = re.findall(r'-p ([a-z0-9-]+)', command)
            self.assertTrue(names, command)
            self.assertTrue(set(names) <= selector.SDK_PACKAGES, command)

    def test_source_alias_cannot_be_a_separate_test_input(self):
        _, path, source = self.standalone_fixture()
        path.unlink()
        path.symlink_to(source)
        self.assertTrue(self.check(str(path.relative_to(self.root))))

    def test_sdk_test_directory_alias_retains_its_dependency_fixture(self):
        _, path, _ = self.standalone_fixture()
        (path.parent / 'mod.rs').write_text('mod native_flow;\n')
        sdk_tests = self.root / 'crates/dev/tilo-app-cli/tests'
        sdk_tests.mkdir()
        (sdk_tests / 'it.rs').write_text('mod shared;\n')
        (sdk_tests / 'shared').symlink_to(path.parent, target_is_directory=True)
        self.assertTrue(self.check(str(path.relative_to(self.root))))

    def test_sdk_test_extensionless_or_broken_alias_retains_lanes(self):
        _, path, _ = self.standalone_fixture()
        sdk_tests = self.root / 'crates/dev/tilo-app-cli/tests'
        sdk_tests.mkdir()
        (sdk_tests / 'it.rs').write_text('const X: &str = include_str!("fixture");\n')
        alias = sdk_tests / 'fixture'
        alias.symlink_to(path)
        self.assertTrue(self.check(str(path.relative_to(self.root))))
        alias.unlink()
        alias.symlink_to(path.parent / 'missing')
        self.assertTrue(self.check(str(path.relative_to(self.root))))

    def test_repository_wide_inputs_run_the_lanes(self):
        self.assertTrue(self.check('rust-toolchain.toml'))
        self.assertTrue(self.check('.forgejo/workflows/app-sdk.yml'))
        self.assertTrue(self.check('crates/stray-file.rs'))
        self.assertTrue(self.check('tools/ci/app_sdk_affected.py'))
        self.assertTrue(self.check('tools/ci/workspace_graph.py'))
        self.assertTrue(self.check('tools/ci/sdk_tool_artifact.py'))


if __name__ == '__main__':
    unittest.main()
