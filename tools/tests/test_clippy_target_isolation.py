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
            def git(*args):
                return subprocess.check_output(['git', '-C', str(root), *args], text=True).strip()
            git('init', '-q')
            git('config', 'user.name', 'Fixture')
            git('config', 'user.email', 'fixture@example.invalid')
            (root / 'lib.rs').write_text('base\n')
            git('add', 'lib.rs', 'tools/ci/clippy-diff.sh')
            git('commit', '-qm', 'base')
            base = git('rev-parse', 'HEAD')
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
 print(json.dumps({'packages':[{'name':'fixture'}]})); sys.exit(0)
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
''')
            cargo.chmod(0o755)
            calls = Path(tmp) / 'calls.jsonl'
            env = dict(os.environ, PATH=str(fake_bin)+os.pathsep+os.environ['PATH'],
                       CALLS=str(calls), FIXTURE_ROOT=str(root), MODE=mode,
                       CARGO_TARGET_DIR=str(Path(tmp) / 'must-not-be-used'))
            env.pop('SCCACHE_RECACHE', None)
            result = subprocess.run(['bash', str(root / 'tools/ci/clippy-diff.sh'), base, '-p', 'fixture'],
                                    cwd=root, env=env, text=True, capture_output=True)
            self.assertEqual(result.returncode, 1 if mode == 'introduced' else 0, result.stderr)
            records = [json.loads(line) for line in calls.read_text().splitlines()]
            self.assertTrue(all(r['target_env'] is None for r in records))
            self.assertEqual(sentinel.read_text(), 'keep')
            self.assertTrue((root / 'target/built-artifact').exists())
            base_calls = [r for r in records if r['cwd'] != str(root)]
            self.assertEqual(len(base_calls), 1)
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


if __name__ == '__main__':
    unittest.main()
