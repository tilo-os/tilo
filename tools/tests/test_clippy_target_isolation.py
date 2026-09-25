"""Run the real comparison script with real Git worktrees and a Cargo fixture."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / 'ci/clippy-diff.sh'


class ClippyTargetIsolationTests(unittest.TestCase):
    def exercise(self, mode):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / 'repo'
            root.mkdir()
            (root / 'tools/ci').mkdir(parents=True)
            shutil.copyfile(SCRIPT, root / 'tools/ci/clippy-diff.sh')
            shutil.copyfile(SCRIPT.parent / 'compiler_inputs.py', root / 'tools/ci/compiler_inputs.py')
            shutil.copyfile(SCRIPT.parent / 'clippy_diagnostics.py', root / 'tools/ci/clippy_diagnostics.py')
            def git(*args):
                return subprocess.check_output(['git', '-C', str(root), *args], text=True).strip()
            git('init', '-q')
            git('config', 'user.name', 'Fixture')
            git('config', 'user.email', 'fixture@example.invalid')
            (root / 'Cargo.toml').write_text('[package]\nname = "fixture"\nversion = "0.1.0"\n')
            source_name = 'old.rs' if mode == 'rename' else 'lib.rs'
            initial_source = 'common\nbase\n' if mode == 'rename' else 'base\n'
            (root / source_name).write_text(initial_source)
            git('add', 'Cargo.toml', source_name, 'tools/ci/clippy-diff.sh', 'tools/ci/compiler_inputs.py',
                'tools/ci/clippy_diagnostics.py')
            git('commit', '-qm', 'base')
            base = git('rev-parse', 'HEAD')
            if mode == 'rename':
                git('mv', 'old.rs', 'new.rs')
                (root / 'new.rs').write_text('common\nhead\n')
            else:
                (root / 'lib.rs').write_text('head\n')
            git('commit', '-qam', 'head')
            (root / 'target').mkdir()
            sentinel = root / 'target/keep-existing-artifact'
            sentinel.write_text('keep')
            fake_bin = Path(tmp) / 'bin'
            fake_bin.mkdir()
            cargo = fake_bin / 'cargo'
            cargo.write_text('''#!/usr/bin/env python3
import json,os,pathlib,sys
if sys.argv[1] == 'metadata':
 manifest=pathlib.Path(sys.argv[sys.argv.index('--manifest-path')+1]).resolve() if '--manifest-path' in sys.argv else pathlib.Path.cwd()/'Cargo.toml'
 tree=manifest.parent
 name='renamed-fixture' if os.environ['MODE']=='package_rename' and str(tree)==os.environ['FIXTURE_ROOT'] else 'fixture'
 print(json.dumps({'workspace_members':[], 'packages':[{'name':name, 'id':name, 'manifest_path':str(manifest)}]})); sys.exit(0)
assert sys.argv[1] == 'clippy'
cwd=pathlib.Path.cwd()
with open(os.environ['CALLS'],'a') as f:
 f.write(json.dumps({'cwd':str(cwd),'target_env':os.environ.get('CARGO_TARGET_DIR'),'recache':os.environ.get('SCCACHE_RECACHE'),'args':sys.argv[1:]})+'\\n')
(cwd/'target').mkdir(exist_ok=True)
(cwd/'target/built-artifact').write_text('built')
is_head=str(cwd)==os.environ['FIXTURE_ROOT']
if is_head and os.environ['MODE']=='retry' and not os.environ.get('SCCACHE_RECACHE'):
 sys.exit(2)
if is_head and os.environ['MODE']=='introduced':
 print(json.dumps({'reason':'compiler-message','message':{'level':'warning','code':{'code':'clippy::fixture'},'message':'new warning','spans':[{'is_primary':True,'file_name':'lib.rs','line_start':1,'column_start':1,'text':[{'text':'head'}]}]}}))
if os.environ['MODE']=='edited_warning':
 source='head' if is_head else 'base'
 print(json.dumps({'reason':'compiler-message','message':{'level':'warning','code':{'code':'clippy::fixture'},'message':'existing warning','spans':[{'is_primary':True,'file_name':'lib.rs','line_start':1,'column_start':1,'text':[{'text':source}]}]}}))
if os.environ['MODE']=='rename':
 file_name='nested/../new.rs' if is_head else 'old.rs'
 source='head' if is_head else 'base'
 print(json.dumps({'reason':'compiler-message','message':{'level':'warning','code':{'code':'clippy::fixture'},'message':'existing warning','spans':[{'is_primary':True,'file_name':file_name,'line_start':1,'column_start':1,'text':[{'text':source}]}]}}))
''')
            cargo.chmod(0o755)
            rustc = fake_bin / 'rustc'
            rustc.write_text('#!/bin/sh\necho fixture-rustc\n')
            rustc.chmod(0o755)
            calls = Path(tmp) / 'calls.jsonl'
            env = dict(os.environ, PATH=str(fake_bin)+os.pathsep+os.environ['PATH'],
                       CALLS=str(calls), FIXTURE_ROOT=str(root), MODE=mode,
                       CARGO_TARGET_DIR=str(Path(tmp) / 'must-not-be-used'))
            env.pop('SCCACHE_RECACHE', None)
            requested_package = 'renamed-fixture' if mode == 'package_rename' else 'fixture'
            result = subprocess.run(['bash', str(root / 'tools/ci/clippy-diff.sh'), base, '-p', requested_package],
                                    cwd=root, env=env, text=True, capture_output=True)
            self.assertEqual(result.returncode, 1 if mode == 'introduced' else 0, result.stderr)
            records = [json.loads(line) for line in calls.read_text().splitlines()]
            self.assertTrue(all(r['target_env'] is None for r in records))
            self.assertEqual(sentinel.read_text(), 'keep')
            self.assertTrue((root / 'target/built-artifact').exists())
            head_receipt = json.loads((root / 'target/ci/clippy-head-inputs.json').read_text())
            base_receipt = json.loads((root / 'target/ci/clippy-base-inputs.json').read_text())
            self.assertEqual(head_receipt['binding']['commit'], git('rev-parse', 'HEAD'))
            self.assertEqual(base_receipt['binding']['commit'], base)
            self.assertEqual(head_receipt['binding']['expected_commit'], head_receipt['binding']['commit'])
            self.assertEqual(base_receipt['binding']['expected_commit'], base)
            self.assertEqual(head_receipt['cargo_exit_code'], 2 if mode == 'retry' else 0)
            self.assertEqual(base_receipt['cargo_exit_code'], 0)
            if mode == 'retry':
                retried = json.loads((root / 'target/ci/clippy-head-retry-inputs.json').read_text())
                self.assertEqual(retried['cargo_exit_code'], 0)
                self.assertIn('Cargo invocation failed; target coverage incomplete', head_receipt['unknown'])
            base_calls = [r for r in records if r['cwd'] != str(root)]
            self.assertEqual(len(base_calls), 1)
            if mode == 'package_rename':
                self.assertIn('fixture', base_calls[0]['args'])
            self.assertFalse(Path(base_calls[0]['cwd']).exists())
            self.assertEqual(len(records), 3 if mode == 'retry' else 2)
            if mode == 'retry':
                self.assertEqual(records[1]['recache'], '1')
            if mode == 'introduced':
                self.assertIn('introduced 1 Clippy diagnostic site', result.stderr)

    def test_distinct_default_targets_preserve_head_artifacts(self):
        self.exercise('clean')

    def test_new_warning_still_fails_without_deleting_head_target(self):
        self.exercise('introduced')

    def test_cache_bypassing_retry_keeps_head_target(self):
        self.exercise('retry')

    def test_renamed_warning_site_keeps_its_baseline(self):
        self.exercise('rename')

    def test_edited_warning_site_keeps_its_baseline(self):
        self.exercise('edited_warning')

    def test_renamed_package_uses_old_package_as_its_baseline(self):
        self.exercise('package_rename')


if __name__ == '__main__':
    unittest.main()
