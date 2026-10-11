"""An old selector cannot bypass the stable launcher's no-Cargo contract."""
import json
import os
import re
from pathlib import Path
import subprocess
import sys
import unittest

import test_current_source_metadata as fixture

SCRIPT = Path(__file__).resolve().parents[1] / 'ci/offline_affected_plan.py'


class GuardedAffectedPlanTests(unittest.TestCase):
    def setUp(self):
        fixture.CurrentSourceMetadataTests.setUp(self)
        self.marker = Path(self.temp.name) / 'cargo-called'
        self.fake_cargo = Path(self.temp.name) / 'cargo'
        self.fake_cargo.write_text('#!/bin/sh\nprintf called > "$CARGO_CALL_MARKER"\nexit 97\n')
        self.fake_cargo.chmod(0o755)
        self.bundle = Path(self.temp.name) / 'bundle.json'
        self.selector = self.root / 'tools/ci/affected_packages.py'
        self.selector.parent.mkdir(parents=True)

    def install(self, source):
        self.selector.write_text(source)
        subprocess.run(['git', '-C', str(self.root), 'add', 'tools'], check=True)
        subprocess.run(['git', '-C', str(self.root), '-c', 'user.name=Fixture', '-c',
                        'user.email=fixture@example.invalid', '-c', 'commit.gpgsign=false',
                        'commit', '-qm', 'old selector'], check=True)
        self.bundle.write_text(json.dumps(fixture.module.seal(self.root, self.metadata)))

    def invoke(self):
        return subprocess.run([sys.executable, str(SCRIPT), '--root', str(self.root),
                               '--bundle', str(self.bundle), '--base', 'origin/main'],
                              env=os.environ | {'CARGO_CALL_MARKER': str(self.marker)},
                              text=True, capture_output=True)

    def test_old_metadata_consumer_gets_complete_remapped_graph_and_original_status(self):
        self.install('import json,sys\nfrom pathlib import Path\n'
                     'p=Path(sys.argv[sys.argv.index("--metadata")+1])\n'
                     'm=json.loads(p.read_text())\n'
                     'assert m["workspace_root"] == str(Path.cwd())\n'
                     'assert len(m["packages"]) == len(m["workspace_members"]) == 1\n'
                     'assert m["packages"][0]["dependencies"][0]["optional"]\n'
                     'print(json.dumps({"security_required":True,"complete":True}))\n'
                     'raise SystemExit(7)\n')
        instructions = (SCRIPT.parents[2] / 'AGENTS.md').read_text()
        command = re.search(r'`(python3 "\$\{TILO_CI_TOOLS:[^`]+)`', instructions)
        self.assertIsNotNone(command, 'the actual prescribed caller must use the stable guard')
        environment = os.environ | {'TILO_CI_TOOLS': str(SCRIPT.parent),
                                    'TILO_CURRENT_WORKTREE_METADATA_BUNDLE': str(self.bundle)}
        result = subprocess.run(['bash', '-euo', 'pipefail', '-c', command[1]], cwd=self.root,
                                env=environment, text=True, capture_output=True)
        self.assertEqual(result.returncode, 7, result.stderr)
        self.assertTrue(json.loads(result.stdout)['security_required'])
        self.assertFalse(self.marker.exists())

    def test_absolute_cargo_fallback_is_blocked_and_the_control_can_observe_it(self):
        self.install('import subprocess\nraise SystemExit(subprocess.call(['
                     + repr(str(self.fake_cargo)) + ',"metadata"]))\n')
        result = self.invoke()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('permits only Git subprocesses', result.stderr)
        self.assertFalse(self.marker.exists())
        # A direct old entrypoint genuinely reaches our fake Cargo. The
        # launcher, rather than a permissive fixture, prevents that fallback.
        control = subprocess.run([sys.executable, str(self.selector)],
                                 env=os.environ | {'CARGO_CALL_MARKER': str(self.marker)})
        self.assertEqual(control.returncode, 97)
        self.assertEqual(self.marker.read_text(), 'called')

    def test_shell_fallback_is_blocked_before_execution(self):
        self.install('import os\nos.system(' + repr(str(self.fake_cargo) + ' metadata') + ')\n')
        result = self.invoke()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('cannot execute shell commands', result.stderr)
        self.assertFalse(self.marker.exists())

    def test_source_mutation_is_rejected_before_old_selector_starts(self):
        self.install('import subprocess\nsubprocess.call([' + repr(str(self.fake_cargo)) + '])\n')
        (self.root / 'src/lib.rs').write_text('// changed source\n')
        result = self.invoke()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('clean worktree', result.stderr)
        self.assertFalse(self.marker.exists())


if __name__ == '__main__':
    unittest.main()
