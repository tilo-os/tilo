"""Keep the real profile-vault browser regression required and fail closed."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from hang import HANG_BOUND

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools" / "ci"))
DRIVER = ROOT / 'crates/system/accounts/web/run.mjs'


class AccountsWebStorageTests(unittest.TestCase):

    def run_without_subject(self, missing):
        # Run the real driver without building: fake only tool provisioning.
        # Missing subjects must yield a failing process, not a successful skip.
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            tools = root / 'bin'
            tools.mkdir()
            cargo = tools / 'cargo'
            cargo.write_text('#!/usr/bin/env python3\nimport json\nprint(json.dumps('
                             + repr({'target_directory': str(root / 'target')}) + '))\n')
            cargo.chmod(0o755)
            bindgen = tools / 'bindgen'
            bindgen.write_text('#!/bin/sh\nexit 0\n')
            bindgen.chmod(0o755)
            env = dict(os.environ, PATH=str(tools) + os.pathsep + os.environ['PATH'],
                       HOME=str(root), TMPDIR=str(root), CHROME='',
                       WASM_BINDGEN=str(bindgen if missing == 'browser' else root / 'missing-wasm-bindgen'))
            result = subprocess.run(['node', str(DRIVER), '--no-build'], cwd=ROOT,
                                    env=env, capture_output=True, text=True, timeout=HANG_BOUND)
            self.assertNotEqual(result.returncode, 0, result.stdout)
            self.assertNotIn('ok profile-bound', result.stdout)
            self.assertIn('no Chrome' if missing == 'browser' else 'missing-wasm-bindgen',
                          result.stderr)
            self.assertFalse(list(root.glob('accounts-storage-*')))

    def test_missing_wasm_tool_fails(self):
        self.run_without_subject('wasm')

    def test_missing_browser_fails_without_leaving_server(self):
        self.run_without_subject('browser')


if __name__ == '__main__':
    unittest.main()
