"""Exercise Clippy threshold normalization and the cached comparison base.

The end-to-end cases run the real clippy-diff.sh against a Git fixture. A fake
`cargo clippy` derives diagnostics from `WARN <lint> <message>` source lines, so
the base checkout, the recorded workspace cache and HEAD all report exactly
what their source text contains.
"""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

CI = Path(__file__).resolve().parents[1] / 'ci'
SPEC = importlib.util.spec_from_file_location('clippy_diagnostics', CI / 'clippy_diagnostics.py')
diagnostics = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(diagnostics)

FAKE_CARGO = r'''#!/usr/bin/env python3
import json, os, pathlib, sys
args = sys.argv[1:]
if args[0] == 'metadata':
    manifest = (pathlib.Path(args[args.index('--manifest-path') + 1]) if '--manifest-path' in args
                else pathlib.Path.cwd() / 'Cargo.toml').resolve()
    package_id = 'path+file://' + str(manifest.parent) + '#fixture@0.1.0'
    data = {'workspace_members': [package_id],
            'packages': [{'name': 'fixture', 'id': package_id, 'manifest_path': str(manifest)}]}
    if '--no-deps' not in args:
        data['resolve'] = {'nodes': [{'id': package_id, 'deps': []}]}
    print(json.dumps(data))
    sys.exit(0)
if args[:2] == ['clippy', '-V']:
    print('clippy 0.1.97 (fixture)')
    sys.exit(0)
assert args[0] == 'clippy'
cwd = pathlib.Path.cwd()
with open(os.environ['CALLS'], 'a') as calls:
    calls.write(json.dumps({'cwd': str(cwd), 'args': args}) + '\n')
package_id = 'path+file://' + str(cwd) + '#fixture@0.1.0'
for path in sorted(cwd.glob('*.rs')):
    for number, line in enumerate(path.read_text().splitlines(), 1):
        if line.startswith('ERROR'):
            print(json.dumps({'reason': 'compiler-message', 'package_id': package_id,
                              'message': {'level': 'error', 'message': 'broken', 'rendered': 'error: broken', 'spans': []}}))
            sys.exit(101)
        if not line.startswith('WARN '):
            continue
        _, code, message = line.split(' ', 2)
        print(json.dumps({'reason': 'compiler-message', 'package_id': package_id, 'message': {
            'level': 'warning', 'code': {'code': 'clippy::' + code}, 'message': message,
            'rendered': f'warning: {message}\n  --> {path.name}:{number}:1',
            'spans': [{'is_primary': True, 'file_name': path.name, 'line_start': number, 'column_start': 1}]}}))
print(json.dumps({'reason': 'compiler-artifact', 'package_id': package_id}))
print(json.dumps({'reason': 'build-finished', 'success': True}))
'''

BASE_SOURCE = (
    'WARN too_many_lines this function has too many lines (104/100)\n'
    'WARN doc_markdown item in documentation is missing backticks\n'
)


class Fixture:
    def __init__(self, tmp):
        self.tmp = Path(tmp)
        self.root = self.tmp / 'repo'
        (self.root / 'tools/ci').mkdir(parents=True)
        for name in ('clippy-diff.sh', 'compiler_inputs.py', 'clippy_diagnostics.py'):
            shutil.copyfile(CI / name, self.root / 'tools/ci' / name)
        (self.root / '.gitignore').write_text('target/\n')
        (self.root / 'Cargo.toml').write_text('[package]\nname = "fixture"\nversion = "0.1.0"\n')
        (self.root / 'lib.rs').write_text(BASE_SOURCE)
        bin_dir = self.tmp / 'bin'
        bin_dir.mkdir()
        (bin_dir / 'cargo').write_text(FAKE_CARGO)
        (bin_dir / 'rustc').write_text('#!/bin/sh\nprintf "rustc 1.97.1 (fixture)\\nhost: x86_64-unknown-linux-gnu\\n"\n')
        for tool in bin_dir.iterdir():
            tool.chmod(0o755)
        self.calls = self.tmp / 'calls.jsonl'
        self.env = dict(os.environ, PATH=str(bin_dir) + os.pathsep + os.environ['PATH'], CALLS=str(self.calls))
        for name in ('CLIPPY_BASE_CACHE', 'CLIPPY_BASE_CACHE_KEY', 'CARGO_TARGET_DIR', 'SCCACHE_RECACHE'):
            self.env.pop(name, None)
        self.git('init', '-q')
        self.git('config', 'user.name', 'Fixture')
        self.git('config', 'user.email', 'fixture@example.invalid')
        self.git('add', '.')
        self.git('commit', '-qm', 'base')
        self.base = self.git('rev-parse', 'HEAD')

    def git(self, *args):
        return subprocess.check_output(['git', '-C', str(self.root), *args], text=True).strip()

    def script(self, *args, **env):
        return subprocess.run(['bash', str(self.root / 'tools/ci/clippy-diff.sh'), *args], cwd=self.root,
                              env=dict(self.env, **env), text=True, capture_output=True)

    def export(self, key='fixture-key'):
        cache = self.tmp / 'cache' / 'base.json'
        result = self.script('--export-base', str(cache), key)
        return result, cache

    def commit_head(self, source):
        (self.root / 'lib.rs').write_text(source)
        self.git('commit', '-q', '--allow-empty', '-am', 'head')

    def records(self):
        if not self.calls.exists():
            return []
        return [json.loads(line) for line in self.calls.read_text().splitlines()]

    def base_calls(self):
        return [call for call in self.records() if call['cwd'] != str(self.root)]


class ThresholdNormalizationTests(unittest.TestCase):
    def test_threshold_lint_messages_drop_measurements(self):
        self.assertEqual(
            diagnostics.normalized_message('clippy::too_many_lines', 'this function has too many lines (104/100)'),
            'this function has too many lines (#/#)')
        self.assertEqual(
            diagnostics.normalized_message('clippy::cognitive_complexity',
                                           'the function has a cognitive complexity of (31/25)'),
            'the function has a cognitive complexity of (#/#)')
        self.assertEqual(diagnostics.normalized_message('clippy::doc_markdown', 'item 2 is missing'),
                         'item 2 is missing')

    def test_growing_function_already_over_the_limit_is_not_new(self):
        with tempfile.TemporaryDirectory() as tmp:
            fixture = Fixture(tmp)
            fixture.commit_head(BASE_SOURCE.replace('104/100', '131/100'))
            result = fixture.script(fixture.base, '-p', 'fixture')
            self.assertEqual(result.returncode, 0, result.stderr)

    def test_function_newly_crossing_the_limit_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            fixture = Fixture(tmp)
            fixture.commit_head(BASE_SOURCE + 'WARN too_many_lines this function has too many lines (101/100)\n')
            result = fixture.script(fixture.base, '-p', 'fixture')
            self.assertEqual(result.returncode, 1)
            self.assertIn('introduced 1 Clippy diagnostic site', result.stderr)
            self.assertIn('(101/100)', result.stderr)


class CachedBaseTests(unittest.TestCase):
    def run_with_cache(self, head_source, *, key='fixture-key', env_key='fixture-key', export_at_head=False):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        fixture = Fixture(tmp.name)
        if not export_at_head:
            exported, cache = fixture.export(key)
            self.assertEqual(exported.returncode, 0, exported.stderr)
        fixture.commit_head(head_source)
        if export_at_head:
            exported, cache = fixture.export(key)
            self.assertEqual(exported.returncode, 0, exported.stderr)
        fixture.calls.unlink()
        result = fixture.script(fixture.base, '-p', 'fixture', CLIPPY_BASE_CACHE=str(cache),
                                CLIPPY_BASE_CACHE_KEY=env_key)
        return fixture, result

    def test_export_records_workspace_sites_for_the_exact_commit(self):
        with tempfile.TemporaryDirectory() as tmp:
            fixture = Fixture(tmp)
            result, cache_path = fixture.export()
            self.assertEqual(result.returncode, 0, result.stderr)
            cache = json.loads(cache_path.read_text())
            self.assertEqual(cache['commit'], fixture.base)
            self.assertEqual(cache['key'], 'fixture-key')
            self.assertEqual([site['file'] for site in cache['sites']], ['lib.rs', 'lib.rs'])
            self.assertEqual([package['manifest'] for package in cache['packages']], ['Cargo.toml'])
            self.assertIn('--workspace', fixture.records()[0]['args'])

    def test_export_refuses_a_modified_tree(self):
        with tempfile.TemporaryDirectory() as tmp:
            fixture = Fixture(tmp)
            (fixture.root / 'lib.rs').write_text('changed\n')
            result, cache = fixture.export()
            self.assertEqual(result.returncode, 2)
            self.assertFalse(cache.exists())

    def test_export_records_nothing_when_the_workspace_does_not_compile(self):
        with tempfile.TemporaryDirectory() as tmp:
            fixture = Fixture(tmp)
            fixture.commit_head('ERROR\n')
            result, cache = fixture.export()
            self.assertEqual(result.returncode, 1)
            self.assertFalse(cache.exists())

    def test_cache_hit_skips_the_base_checkout(self):
        fixture, result = self.run_with_cache(BASE_SOURCE.replace('104/100', '120/100') + 'fn unrelated() {}\n')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('Using cached Clippy diagnostics', result.stdout)
        self.assertIn('at cached base', result.stdout)
        self.assertEqual(fixture.base_calls(), [])
        self.assertEqual(len(fixture.records()), 1)
        self.assertFalse((fixture.root / 'target/ci/clippy-base-inputs.json').exists())

    def test_cache_hit_with_new_site_is_confirmed_against_a_fresh_base(self):
        fixture, result = self.run_with_cache(BASE_SOURCE + 'WARN doc_markdown item in documentation is missing backticks\n')
        self.assertEqual(result.returncode, 1)
        self.assertIn('confirming against a fresh base checkout', result.stderr)
        self.assertIn('introduced 1 Clippy diagnostic site', result.stderr)
        self.assertEqual(len(fixture.base_calls()), 1)

    def test_mismatched_fingerprint_falls_back(self):
        fixture, result = self.run_with_cache(BASE_SOURCE, env_key='other-toolchain')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('fingerprint differs', result.stdout)
        self.assertEqual(len(fixture.base_calls()), 1)

    def test_cache_for_another_commit_falls_back(self):
        fixture, result = self.run_with_cache(BASE_SOURCE + 'WARN doc_markdown new\n', export_at_head=True)
        # A cache recorded at HEAD would hide the new site; it must be rejected.
        self.assertEqual(result.returncode, 1)
        self.assertIn('Cached Clippy base unusable (recorded for', result.stdout)
        self.assertEqual(len(fixture.base_calls()), 1)

    def test_missing_cache_file_falls_back(self):
        with tempfile.TemporaryDirectory() as tmp:
            fixture = Fixture(tmp)
            fixture.commit_head(BASE_SOURCE)
            result = fixture.script(fixture.base, '-p', 'fixture', CLIPPY_BASE_CACHE=str(fixture.tmp / 'absent.json'),
                                    CLIPPY_BASE_CACHE_KEY='fixture-key')
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn('no cache recorded for this base', result.stdout)
            self.assertEqual(len(fixture.base_calls()), 1)


class CacheSelectionTests(unittest.TestCase):
    def test_lint_closure_follows_what_cargo_clippy_builds(self):
        def dep(name, *kinds):
            return {'pkg': name, 'dep_kinds': [{'kind': kind, 'target': None} for kind in kinds]}
        metadata = {
            'workspace_members': ['a', 'b', 'c', 'd', 'e', 'unrelated'],
            'resolve': {'nodes': [
                {'id': 'a', 'deps': [dep('b', 'dev'), dep('registry-crate', None)]},
                {'id': 'b', 'deps': [dep('c', None), dep('d', 'dev')]},
                {'id': 'c', 'deps': [dep('e', 'build')]},
                {'id': 'd', 'deps': []},
                {'id': 'e', 'deps': []},
                {'id': 'unrelated', 'deps': [dep('a', None)]},
            ]},
        }
        closures = diagnostics.lint_closures(metadata)
        self.assertEqual(closures['a'], ['a', 'b', 'c', 'e'])
        self.assertEqual(closures['b'], ['b', 'c', 'd', 'e'])
        # A dependant builds `a` as a library only, without its dev-dependencies.
        self.assertEqual(closures['unrelated'], ['a', 'unrelated'])

    def test_cached_sites_are_limited_to_linted_packages(self):
        cache = {
            'packages': [{'name': 'a', 'id': 'a-id'}, {'name': 'b', 'id': 'b-id'}, {'name': 'c', 'id': 'c-id'}],
            'lint_closures': {'a-id': ['a-id', 'b-id'], 'b-id': ['b-id'], 'c-id': ['c-id']},
            'sites': [{'package': 'a-id'}, {'package': 'b-id'}, {'package': 'c-id'}],
        }
        self.assertEqual(diagnostics.cached_base_sites(cache, ['a']), [{'package': 'a-id'}, {'package': 'b-id'}])
        with self.assertRaises(SystemExit):
            diagnostics.cached_base_sites(cache, ['missing'])

    def test_cache_key_binds_commit_toolchain_image_and_flags(self):
        with tempfile.TemporaryDirectory() as tmp:
            bin_dir = Path(tmp)
            (bin_dir / 'rustc').write_text('#!/bin/sh\necho "rustc ${FIXTURE_RUSTC:-1.97.1}"\n')
            (bin_dir / 'cargo').write_text('#!/bin/sh\necho "clippy 0.1.97"\n')
            for tool in bin_dir.iterdir():
                tool.chmod(0o755)
            env = dict(os.environ, PATH=str(bin_dir) + os.pathsep + os.environ['PATH'], RUSTFLAGS='-D warnings')

            def key(base='a' * 40, image='builder@sha256:1', **overrides):
                return subprocess.check_output(
                    ['python3', str(CI / 'clippy_diagnostics.py'), 'cache-key', '--base', base, '--image', image],
                    env=dict(env, **overrides), text=True).strip()

            reference = key()
            self.assertTrue(reference.startswith('clippy-base-v1-' + 'a' * 40 + '-'))
            self.assertEqual(reference, key())
            self.assertNotEqual(reference, key(base='b' * 40))
            self.assertNotEqual(reference, key(image='builder@sha256:2'))
            self.assertNotEqual(reference, key(RUSTFLAGS=''))
            self.assertNotEqual(reference, key(FIXTURE_RUSTC='1.98.0'))


if __name__ == '__main__':
    unittest.main()
