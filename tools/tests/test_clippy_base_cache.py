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


def packages(root):
    # The root package plus one member per subdirectory holding a Cargo.toml.
    found = [('fixture', root, '')]
    found += [(manifest.parent.name, manifest.parent, manifest.parent.name + '/')
              for manifest in sorted(root.glob('*/Cargo.toml'))]
    return [(name, directory, prefix, 'path+file://' + str(directory) + '#' + name + '@0.1.0')
            for name, directory, prefix in found]


if args[0] == 'metadata':
    manifest = (pathlib.Path(args[args.index('--manifest-path') + 1]) if '--manifest-path' in args
                else pathlib.Path.cwd() / 'Cargo.toml').resolve()
    members = packages(manifest.parent)
    data = {'workspace_members': [package_id for *_, package_id in members],
            'packages': [{'name': name, 'id': package_id, 'manifest_path': str(directory / 'Cargo.toml')}
                         for name, directory, _, package_id in members]}
    if '--no-deps' not in args:
        data['resolve'] = {'nodes': [{'id': package_id, 'deps': []} for *_, package_id in members]}
    print(json.dumps(data))
    sys.exit(0)
if args[:2] == ['clippy', '-V']:
    print('clippy 0.1.97 (fixture)')
    sys.exit(0)
assert args[0] == 'clippy'
cwd = pathlib.Path.cwd()
with open(os.environ['CALLS'], 'a') as calls:
    calls.write(json.dumps({'cwd': str(cwd), 'args': args}) + '\n')
selected = {args[index + 1] for index, arg in enumerate(args) if arg in ('-p', '--package')}
for name, directory, prefix, package_id in packages(cwd):
    if '--workspace' not in args and name not in selected:
        continue
    for path in sorted(directory.glob('*.rs')):
        file_name = prefix + path.name
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
                'rendered': f'warning: {message}\n  --> {file_name}:{number}:1',
                'spans': [{'is_primary': True, 'file_name': file_name, 'line_start': number, 'column_start': 1}]}}))
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


SELECTOR_STUB = r'''#!/usr/bin/env python3
import json, os, sys
with open(os.environ['SELECTOR_ARGS'], 'w') as out:
    json.dump(sys.argv[1:], out)
print('{}')
'''


class ComparisonBaseTests(unittest.TestCase):
    """Run check.yml's production selection step, then Clippy against its base.

    Main has fixed a warning after a pull request branched. The branch still
    contains the old line, which the PR never touched, so it must not fail.
    """

    def setUp(self):
        from test_ci_check_aggregate import step_script
        workflow = (CI.parents[1] / '.forgejo/workflows/check.yml').read_text()
        self.plan_script = step_script(workflow, 'plan')
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.fixture = fixture = Fixture(tmp.name)
        (fixture.root / 'tools/ci/affected_packages.py').write_text(SELECTOR_STUB)
        (fixture.root / '.gitignore').write_text('target/\nci-plan.json\n')
        fixture.git('add', '.')
        fixture.git('commit', '-qm', 'selector')
        self.branch_point = fixture.git('rev-parse', 'HEAD')
        fixture.commit_head(BASE_SOURCE.replace('WARN doc_markdown', 'fixed doc_markdown'))
        self.main_tip = fixture.git('rev-parse', 'HEAD')
        fixture.git('checkout', '-q', '-b', 'topic', self.branch_point)
        (fixture.root / 'other.rs').write_text('fn unrelated() {}\n')
        fixture.git('add', 'other.rs')
        fixture.git('commit', '-qm', 'topic')
        self.head = fixture.git('rev-parse', 'HEAD')

    def select(self, event, pull_request_base='', before='', ref='refs/pull/1/head'):
        output = self.fixture.tmp / 'github-output'
        output.write_text('')
        arguments = self.fixture.tmp / 'selector-args.json'
        result = subprocess.run(
            ['bash', '-c', self.plan_script], cwd=self.fixture.root, text=True, capture_output=True,
            env=dict(self.fixture.env, EVENT_NAME=event, PULL_REQUEST_BASE=pull_request_base,
                     PUSH_BEFORE=before, GITHUB_REF=ref, GITHUB_OUTPUT=str(output),
                     GITHUB_STEP_SUMMARY=str(self.fixture.tmp / 'summary'),
                     SELECTOR_ARGS=str(arguments)))
        self.assertEqual(result.returncode, 0, result.stderr)
        base = dict(line.split('=', 1) for line in output.read_text().splitlines())['base']
        selector = json.loads(arguments.read_text())
        self.assertEqual(selector[selector.index('--base') + 1], base,
                         'Clippy and package selection must share one comparison base')
        return base

    def test_pull_request_behind_main_compares_with_its_merge_base(self):
        base = self.select('pull_request', pull_request_base=self.main_tip)
        self.assertEqual(base, self.branch_point)
        result = self.fixture.script(base, '-p', 'fixture')
        self.assertEqual(result.returncode, 0, result.stderr)
        # Against main's current tip the untouched old warning looks new.
        stale = self.fixture.script(self.main_tip, '-p', 'fixture')
        self.assertEqual(stale.returncode, 1)
        self.assertIn('introduced 1 Clippy diagnostic site', stale.stderr)

    def test_pull_request_behind_main_reuses_the_record_of_its_merge_base(self):
        fixture = self.fixture
        fixture.git('checkout', '-q', '--detach', self.branch_point)
        exported, cache = fixture.export()
        self.assertEqual(exported.returncode, 0, exported.stderr)
        fixture.git('checkout', '-q', 'topic')
        fixture.calls.unlink()
        base = self.select('pull_request', pull_request_base=self.main_tip)
        result = fixture.script(base, '-p', 'fixture', CLIPPY_BASE_CACHE=str(cache),
                                CLIPPY_BASE_CACHE_KEY='fixture-key')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('Using cached Clippy diagnostics', result.stdout)
        self.assertEqual(fixture.base_calls(), [])

    def test_head_that_merged_newer_main_compares_with_the_merged_tip(self):
        fixture = self.fixture
        fixture.git('merge', '-q', '--no-edit', self.main_tip)
        # Main moves on and fixes the remaining warning after the branch
        # merged it; the branch still carries that warning untouched.
        fixture.git('checkout', '-q', '--detach', self.main_tip)
        fixture.commit_head('fixed too_many_lines\nfixed doc_markdown\n')
        newer_tip = fixture.git('rev-parse', 'HEAD')
        fixture.git('checkout', '-q', 'topic')
        base = self.select('pull_request', pull_request_base=newer_tip)
        self.assertEqual(base, self.main_tip)
        result = fixture.script(base, '-p', 'fixture')
        self.assertEqual(result.returncode, 0, result.stderr)
        stale = fixture.script(newer_tip, '-p', 'fixture')
        self.assertEqual(stale.returncode, 1)

    def test_up_to_date_pull_request_compares_with_the_base_tip(self):
        self.fixture.git('merge', '-q', '--no-edit', self.main_tip)
        self.assertEqual(self.select('pull_request', pull_request_base=self.main_tip), self.main_tip)

    def test_push_to_main_compares_with_the_previous_tip(self):
        fixture = self.fixture
        fixture.git('checkout', '-q', '--detach', self.main_tip)
        fixture.git('merge', '-q', '--no-ff', '--no-edit', 'topic')
        self.assertEqual(self.select('push', before=self.main_tip, ref='refs/heads/main'), self.main_tip)
        head_parent = fixture.git('rev-parse', 'HEAD^')
        self.assertEqual(self.select('push', before='0' * 40, ref='refs/heads/main'), head_parent)


class AffectedSetTests(unittest.TestCase):
    """A change can pull an untouched package into the linted set.

    Its existing warnings are present at the base with the same package
    arguments, so only diagnostics the change itself adds may fail.
    """

    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.fixture = fixture = Fixture(tmp.name)
        dependant = fixture.root / 'dependant'
        dependant.mkdir()
        (dependant / 'Cargo.toml').write_text('[package]\nname = "dependant"\nversion = "0.1.0"\n')
        (dependant / 'lib.rs').write_text('WARN redundant_clone redundant clone\n')
        fixture.git('add', 'dependant')
        fixture.git('commit', '-qm', 'dependant')
        self.base = fixture.git('rev-parse', 'HEAD')
        exported, self.cache = fixture.export()
        self.assertEqual(exported.returncode, 0, exported.stderr)
        (fixture.root / 'other.rs').write_text('fn unrelated() {}\n')
        fixture.git('add', 'other.rs')
        fixture.git('commit', '-qm', 'change the dependency')
        fixture.calls.unlink()

    def test_existing_warning_in_a_newly_selected_package_is_not_new(self):
        result = self.fixture.script(self.base, '-p', 'fixture', '-p', 'dependant')
        self.assertEqual(result.returncode, 0, result.stderr)
        base_runs = self.fixture.base_calls()
        self.assertEqual(len(base_runs), 1)
        self.assertEqual(base_runs[0]['args'][:6], ['clippy', '--locked', '--all-targets', '-p', 'fixture', '-p'])

    def test_recorded_workspace_base_covers_a_newly_selected_package(self):
        result = self.fixture.script(self.base, '-p', 'fixture', '-p', 'dependant',
                                     CLIPPY_BASE_CACHE=str(self.cache), CLIPPY_BASE_CACHE_KEY='fixture-key')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('Using cached Clippy diagnostics', result.stdout)
        self.assertEqual(self.fixture.base_calls(), [])

    def test_new_warning_in_a_newly_selected_package_still_fails(self):
        fixture = self.fixture
        (fixture.root / 'dependant/lib.rs').write_text(
            'WARN redundant_clone redundant clone\nWARN redundant_clone redundant clone\n')
        fixture.git('commit', '-qam', 'another clone')
        for env in ({}, {'CLIPPY_BASE_CACHE': str(self.cache), 'CLIPPY_BASE_CACHE_KEY': 'fixture-key'}):
            with self.subTest(cached=bool(env)):
                result = fixture.script(self.base, '-p', 'fixture', '-p', 'dependant', **env)
                self.assertEqual(result.returncode, 1)
                self.assertIn('1 more `clippy::redundant_clone` site(s) in dependant/lib.rs', result.stderr)

    def test_package_added_by_the_change_has_no_baseline(self):
        fixture = self.fixture
        added = fixture.root / 'added'
        added.mkdir()
        (added / 'Cargo.toml').write_text('[package]\nname = "added"\nversion = "0.1.0"\n')
        (added / 'lib.rs').write_text('WARN redundant_clone redundant clone\n')
        fixture.git('add', 'added')
        fixture.git('commit', '-qm', 'add a package')
        result = fixture.script(self.base, '-p', 'fixture', '-p', 'added')
        self.assertEqual(result.returncode, 1)
        self.assertIn('added/lib.rs', result.stderr)


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
