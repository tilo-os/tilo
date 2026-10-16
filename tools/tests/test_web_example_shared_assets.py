import json
from pathlib import Path
import os
import subprocess
import shutil
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class SharedWebAssets(unittest.TestCase):
    def fixture(self, root, examples):
        (root / 'tools/ci').mkdir(parents=True)
        (root / 'tools/ci/build-web-examples.sh').write_bytes((ROOT / 'tools/ci/build-web-examples.sh').read_bytes())
        runtime = root / 'crates/browser/tilo-browser-runtime'
        (runtime / 'web').mkdir(parents=True)
        (runtime / 'Cargo.toml').write_text('wasm-bindgen = "=0.fixture"\n')
        (root / 'scripts/moq-browser').mkdir(parents=True)
        (root / 'scripts/moq-browser/package-lock.json').write_text('{}')
        (root / 'bin').mkdir()
        (root / 'target/debug').mkdir(parents=True)
        calls = root / 'calls.jsonl'
        common = 'import json,os,pathlib,sys\nroot=pathlib.Path(os.environ["FIXTURE_ROOT"])\n'
        programs = {
            'cargo': common + 'assert sys.argv[1:]==["build","--locked","-p","tilo-app-cli","--bin","tilo-app"]\n',
            'wasm-bindgen': 'print("wasm-bindgen 0.fixture")\n',
            'npm': common + 'assert sys.argv[1:]==["ci","--no-audit","--no-fund","--silent"]\nif os.environ.get("FAIL_NPM")=="1":sys.exit(23)\nwith (root/"calls.jsonl").open("a") as f:f.write(json.dumps({"npm":1})+"\\n")\n',

        }
        node = shutil.which('node')
        if node is None:
            raise RuntimeError('the production JSON selection/reporting checks require Node.js')
        (root / 'bin/node').symlink_to(node)
        (root / 'scripts/moq-browser/build.mjs').write_text("""import fs from 'node:fs';
import path from 'node:path';
const root=process.env.FIXTURE_ROOT;
fs.writeFileSync(path.join(root,'crates/browser/tilo-browser-runtime/web/media-moq-library.mjs'),'export const pinned = true;');
fs.writeFileSync(path.join(root,'crates/browser/tilo-browser-runtime/web/media-moq-library.LICENSE.txt'),'pinned notices');
""")
        cli = common + '''if sys.argv[1:]==["examples","--json"]:
 print(os.environ["FIXTURE_EXAMPLES"])
else:
 assert sys.argv[1:5]==["build","--json","--platform","web"]
 if sys.argv[5].endswith("/"+os.environ.get("FAIL_APP","-")):sys.exit(7)
 assert (root/"crates/browser/tilo-browser-runtime/web/media-moq-library.mjs").read_text()=="export const pinned = true;"
 assert (root/"crates/browser/tilo-browser-runtime/web/media-moq-library.LICENSE.txt").read_text()=="pinned notices"
 with (root/"calls.jsonl").open("a") as f:f.write(json.dumps({"app":sys.argv[5]})+"\\n")
 out=root/"bundles"/pathlib.Path(sys.argv[5]).name
 out.mkdir(parents=True)
 print(json.dumps({"result":{"app_id":"fixture","module_bytes":42,"output":str(out),"entry":"index.html"}}))
'''
        for name, code in programs.items():
            p = root / 'bin' / name
            p.write_text('#!' + os.sys.executable + '\n' + code)
            p.chmod(0o755)
        p = root / 'target/debug/tilo-app'
        p.write_text('#!' + os.sys.executable + '\n' + cli)
        p.chmod(0o755)
        # The script finds tilo-app under CARGO_TARGET_DIR when it is set, which in CI
        # is the node's build directory: the fixture's own target/ is the one to use.
        env = {key: value for key, value in os.environ.items() if key != 'CARGO_TARGET_DIR'}
        return {**env, 'PATH': str(root / 'bin') + ':' + os.environ['PATH'],
                'FIXTURE_ROOT': str(root), 'FIXTURE_EXAMPLES': json.dumps({'result': {'examples': examples}})}, calls

    def test_builds_pinned_shared_assets_once_before_every_selected_example(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            env, calls = self.fixture(root, [{'name': 'counter', 'web': True}, {'name': 'qr-code', 'web': True}])
            result = subprocess.run(['bash', str(root/'tools/ci/build-web-examples.sh'), 'example-counter', 'example-qr-code'], env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual([json.loads(x) for x in calls.read_text().splitlines()],
                             [{'npm': 1}, {'app': 'examples/apps/counter'}, {'app': 'examples/apps/qr-code'}])
            self.assertIn('TILO_WEB_MOQ_BUILDS=1 examples=2', result.stdout)
            self.assertIn('phase=web-example-counter', result.stdout)
            self.assertIn('phase=web-example-qr-code', result.stdout)
            for app in ['counter', 'qr-code']:
                self.assertEqual((root/'bundles'/app/'media-moq-library.LICENSE.txt').read_text(), 'pinned notices')

    def test_later_examples_build_together_and_any_failure_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            names = ['counter', 'qr-code', 'localized', 'data-table']
            env, calls = self.fixture(root, [{'name': name, 'web': True} for name in names])
            env['FAIL_APP'] = 'localized'
            result = subprocess.run(['bash', str(root/'tools/ci/build-web-examples.sh'), *(f'example-{name}' for name in names)], env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertIn('web build failed: localized', result.stderr)
            built = [json.loads(x) for x in calls.read_text().splitlines()]
            self.assertEqual(built[:2], [{'npm': 1}, {'app': 'examples/apps/counter'}])
            self.assertEqual(sorted(call['app'] for call in built[2:]), ['examples/apps/data-table', 'examples/apps/qr-code'])
            for app in ['counter', 'qr-code', 'data-table']:
                self.assertIn(f'phase=web-example-{app}', result.stdout)

    def test_failed_pinned_setup_fails_before_any_example_is_built(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            env, calls = self.fixture(root, [{'name': 'counter', 'web': True}])
            env['FAIL_NPM'] = '1'
            result = subprocess.run(['bash', str(root/'tools/ci/build-web-examples.sh'), 'example-counter'], env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 23, result.stderr)
            self.assertFalse(calls.exists())
            self.assertFalse((root/'bundles').exists())

    def test_empty_selection_does_not_install_or_build_shared_assets(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            env, calls = self.fixture(root, [{'name': 'native', 'web': False}])
            result = subprocess.run(['bash', str(root/'tools/ci/build-web-examples.sh'), 'example-native'], env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(calls.exists())
            self.assertIn('no web-capable example selected', result.stdout)


if __name__ == '__main__':
    unittest.main()
