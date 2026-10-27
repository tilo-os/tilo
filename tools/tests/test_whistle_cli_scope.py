"""A public candidate experiment must not become installation or capture authority."""
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from hang import HANG_BOUND

RELEASE = Path(__file__).resolve().parents[1] / 'release'
sys.path.insert(0, str(RELEASE))
spec = importlib.util.spec_from_file_location('whistle_scope', RELEASE / 'qualify_whistle_cli_scope.py')
scope = importlib.util.module_from_spec(spec)
spec.loader.exec_module(scope)


class WhistleScopeTests(unittest.TestCase):
    def test_scope_is_mandatory_before_any_artifact_read(self):
        result = subprocess.run(
            [sys.executable, str(RELEASE / 'qualify_whistle_cli_scope.py'),
             '--runtime-dir', '/nonexistent', '--manifest', '/nonexistent', '--case', 'fr_fr-0'],
            capture_output=True, text=True, timeout=HANG_BOUND)
        self.assertNotEqual(result.returncode, 0)
        self.assertRegex(result.stderr, 'unprivileged public review only|unverified scope controls')
        self.assertNotIn('WHISTLE_CLI_SCOPE_RESULT', result.stdout)

    def test_candidate_language_boundary_cannot_be_overridden(self):
        result = subprocess.run(
            [sys.executable, str(RELEASE / 'qualify_whistle_cli_scope.py'),
             '--runtime-dir', '/nonexistent', '--manifest', '/nonexistent', '--case', 'ar_eg-0'],
            capture_output=True, text=True, timeout=HANG_BOUND)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('invalid choice', result.stderr)
        self.assertNotIn('WHISTLE_CLI_SCOPE_RESULT', result.stdout)

    def test_fixture_receipt_language_and_uniqueness_precede_file_access(self):
        with tempfile.TemporaryDirectory() as directory:
            manifest = Path(directory) / 'manifest.json'
            row = {'id': 'fr_fr-0', 'language': 'fr',
                   'sha256': scope.PUBLIC_CASES['fr_fr-0'][1],
                   'file': 'private.wav', 'size': 10}
            for changes in [{'dataset': 'private'}, {'revision': 'new'},
                            {'cases': [dict(row, language='ar')]},
                            {'cases': [dict(row, sha256='0' * 64)]},
                            {'cases': [row, row]}]:
                data = {'dataset': 'google/fleurs',
                        'revision': '70bb2e84b976b7e960aa89f1c648e09c59f894dd', 'cases': [row]}
                manifest.write_text(json.dumps(dict(data, **changes)))
                with self.assertRaisesRegex(RuntimeError, 'public corpus|development fixture'):
                    scope.verified_fixture(manifest, 'fr_fr-0')

    def test_runtime_symlink_is_refused_before_untrusted_bytes_are_read(self):
        ledger = json.loads((RELEASE.parents[1] / 'docs/architecture/dictation-whistle-review.json').read_text())
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'needle').symlink_to('/nonexistent-private-runtime')
            with self.assertRaisesRegex(RuntimeError, 'owned regular file'):
                scope.verified_runtime(root, ledger)


if __name__ == '__main__':
    unittest.main()
