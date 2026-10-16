import copy
from pathlib import Path
import subprocess
import sys
import json
import tempfile
import unittest
from tools.apps.dev_app_versions import (
    collection_plan,
    development_version,
    release_version,
    source_identity,
    stage_manifest,
    version_plan,
)

class DevelopmentVersionTests(unittest.TestCase):
    def test_development_versions_are_stable_monotonic_and_bound_to_real_history(self):
        with self.subTest(phase='identity is stable for retry and sequence is numeric'):
            sha = 'a' * 40
            self.assertEqual(development_version('0.1.0', 10, sha), '0.1.1-dev.10+g' + sha)
            for declared in ('0.1', '0.1.0-beta', '01.0.0'):
                with self.assertRaises(ValueError):
                    development_version(declared, 10, sha)
            with self.assertRaises(ValueError):
                development_version('0.1.0', 0, sha)
        with self.subTest(phase='continuous release is stable and monotonic'):
            old = release_version('0.1.0', 41, 'a' * 40)
            new = release_version('0.1.0', 42, 'b' * 40)
            self.assertEqual(old, '0.1.41+g' + 'a' * 40)
            self.assertEqual(new, '0.1.42+g' + 'b' * 40)
            self.assertNotIn('-', new, 'main releases must remain visible on the stable channel')
            self.assertEqual(
                version_plan({'catalog': [{'version': '0.1.0'}]}, 42, 'b' * 40, stable=True)
                ['catalog'][0]['version'],
                new,
            )
        with self.subTest(phase='real history rejects side branch and shallow checkout'):
            with tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp) / 'repo'
                def git(*args):
                    return subprocess.check_output(['git', '-C', str(root), *args], text=True, stderr=subprocess.PIPE).strip()
                root.mkdir()
                git('init', '-b', 'main')
                git('config', 'user.email', 'test@example.invalid')
                git('config', 'user.name', 'test')
                git('commit', '--allow-empty', '-m', 'first')
                first = git('rev-parse', 'HEAD')
                git('commit', '--allow-empty', '-m', 'second')
                second = git('rev-parse', 'HEAD')
                self.assertEqual(source_identity(root, first, 'main'), 1)
                self.assertEqual(source_identity(root, second, 'main'), 2)
                plan = root / 'plan.json'
                plan.write_text(json.dumps({'catalog': [{'version': '0.1.0'}]}))
                helper = Path(__file__).resolve().parents[1] / 'apps/dev_app_versions.py'
                mismatch = subprocess.run([sys.executable, str(helper), '--root', str(root),
                                           '--source', first, '--main-ref', 'main', '--plan', str(plan)],
                                          text=True, capture_output=True)
                self.assertNotEqual(mismatch.returncode, 0)
                self.assertIn('checked-out commit', mismatch.stderr)
                self.assertEqual(json.loads(plan.read_text())['catalog'][0]['version'], '0.1.0')

                git('checkout', '-b', 'side', first)
                git('commit', '--allow-empty', '-m', 'side')
                with self.assertRaisesRegex(ValueError, 'first-parent'):
                    source_identity(root, git('rev-parse', 'HEAD'), 'main')
                git('checkout', 'main')
                shallow = Path(tmp) / 'shallow'
                subprocess.run(['git', 'clone', '--depth=1', root.as_uri(), str(shallow)], check=True, capture_output=True)
                with self.assertRaisesRegex(ValueError, 'complete main history'):
                    source_identity(shallow, second)
        with self.subTest(phase='manifest and plan agree without changing runtime'):
            plan = {'catalog': [{'name': 'calc', 'version': '0.1.0', 'runtime': 'tilo-runtime/1.0.0'}]}
            result = version_plan(plan, 10, 'b' * 40)
            self.assertEqual(plan['catalog'][0]['version'], '0.1.0')
            with tempfile.TemporaryDirectory() as tmp:
                path = Path(tmp) / 'manifest.toml'
                path.write_text('[package]\nversion = "0.1.0"\nruntime = "tilo-runtime/1.0.0"\n[other]\nversion = "9.0.0"\n')
                stage_manifest(path, result['catalog'][0]['version'])
                self.assertIn('version = "0.1.1-dev.10+g', path.read_text())
                self.assertIn('version = "9.0.0"', path.read_text())
                self.assertIn('runtime = "tilo-runtime/1.0.0"', path.read_text())
        with self.subTest(phase='selected identity advances but unchanged version is reused'):
            def app(name):
                return {'name': name, 'id': 'so.tilo.' + name, 'package': name, 'version': '0.1.0', 'runtime': 'tilo-runtime/1.0.0'}
            plan = {'catalog': [app('calc'), app('clock')]}
            old = version_plan(plan, 9, 'a' * 40)
            new = version_plan({'catalog': [app('calc')]}, 10, 'b' * 40)
            merged = collection_plan(plan, {'apps': new['catalog']}, {'apps': old['catalog']})
            self.assertEqual(merged['catalog'][0]['version'], new['catalog'][0]['version'])
            self.assertEqual(merged['catalog'][1]['version'], old['catalog'][1]['version'])
            stale = copy.deepcopy(plan)
            stale['catalog'][1]['version'] = '0.2.0'
            with self.assertRaisesRegex(ValueError, 'rebuild clock'):
                collection_plan(stale, {'apps': new['catalog']}, {'apps': old['catalog']})
        with self.subTest(phase='a packed app names the build of its declared runtime release'):
            built = copy.deepcopy(new['catalog'])
            built[0]['runtime'] = 'tilo-runtime/1.0.0+native.' + '0' * 32
            merged = collection_plan(plan, {'apps': built}, {'apps': old['catalog']})
            self.assertEqual(merged['catalog'][0]['version'], new['catalog'][0]['version'])
            built[0]['runtime'] = 'tilo-runtime/2.0.0+native.' + '0' * 32
            with self.assertRaisesRegex(ValueError, 'calc runtime'):
                collection_plan(plan, {'apps': built}, {'apps': old['catalog']})


if __name__ == '__main__':
    unittest.main()
