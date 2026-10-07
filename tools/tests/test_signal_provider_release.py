"""Protect explicit release admission and refusal before signing or upload."""
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('signal_release', ROOT / 'tools/apps/signal_provider_release.py')
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


class SignalProviderRelease(unittest.TestCase):
    def environment(self):
        return {'SIGNAL_PROVIDER_ONLY': 'true', 'GITHUB_EVENT_NAME': 'workflow_dispatch',
                'GITHUB_REF': 'refs/heads/main', 'GITHUB_SHA': 'a' * 40,
                'REQUESTED_PUBLISH': 'true', 'REQUESTED_ARCHITECTURES': 'x86_64'}

    def test_only_exact_explicit_main_release_is_admitted(self):
        self.assertTrue(release.mode(self.environment(), 'a' * 40))
        for change in ({'GITHUB_EVENT_NAME': 'push'}, {'GITHUB_EVENT_NAME': 'pull_request'},
                       {'GITHUB_REF': 'refs/heads/codex/task'}, {'GITHUB_SHA': 'b' * 40},
                       {'REQUESTED_PUBLISH': 'false'}, {'REQUESTED_ARCHITECTURES': 'aarch64'},
                       {'REQUESTED_ARCHITECTURES': ''}, {'REQUESTED_APPS': 'browser'},
                       {'ENGINE_RUNTIME_ONLY': 'true'}, {'VERIFY_FINAL_RECEIPT': 'true'},
                       {'COORDINATOR_INTENT': 'intent'}, {'REQUESTED_BUILDER': 'other'},
                       {'BASELINE_SHA': 'b' * 40}, {'SIGNAL_PROVIDER_ONLY': 'yes'}):
            with self.subTest(change=change), self.assertRaises(ValueError):
                release.mode(self.environment() | change, 'a' * 40)
        self.assertFalse(release.mode({}, 'a' * 40))

    def compiled(self, root):
        (root / 'bin').mkdir()
        (root / 'bin/tool').write_bytes(b'compiled release tool')
        (root / 'signal-provider').mkdir()
        (root / 'signal-provider/provider').write_bytes(b'compiled pinned provider')
        proof = release.provenance(release.release.head())
        proof['compiled'] = {'bin': release.inventory(root / 'bin'),
                             'payload': release.inventory(root / 'signal-provider')}
        (root / 'build.json').write_text(json.dumps(proof))
        return proof

    def test_missing_original_signing_identities_cannot_generate_replacements(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            proof = self.compiled(root)
            with patch.dict(os.environ, {}, clear=True), \
                    patch.object(release.release, 'head', return_value=proof['source']), \
                    patch.object(release.subprocess, 'check_output') as execute:
                with self.assertRaisesRegex(ValueError, 'none is created'):
                    release.seal(root, proof['source'], '1')
                execute.assert_not_called()

    def test_changed_compiled_bytes_are_refused_before_identity_use(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            proof = self.compiled(root)
            (root / 'signal-provider/provider').write_bytes(b'changed provider')
            with patch.object(release.release, 'head', return_value=proof['source']), \
                    patch.object(release.subprocess, 'check_output') as execute:
                with self.assertRaisesRegex(ValueError, 'bytes changed'):
                    release.seal(root, proof['source'], '1')
                execute.assert_not_called()

    def test_foreign_runtime_signer_is_refused_before_bundle_or_package_writes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            proof = self.compiled(root)
            environment = {'RUNTIME_BUNDLE_IDENTITY_FILE': '/fixture/existing-runtime',
                           'TAPP_PUBLISHER_IDENTITY_FILE': '/fixture/existing-app'}
            with patch.dict(os.environ, environment, clear=True), \
                    patch.object(release.release, 'head', return_value=proof['source']), \
                    patch.object(release.subprocess, 'check_output', return_value='f' * 64), \
                    patch.object(release.subprocess, 'run') as execute:
                with self.assertRaisesRegex(ValueError, 'accepted runtime publisher differs'):
                    release.seal(root, proof['source'], '1')
                execute.assert_not_called()

    def test_inventory_refuses_links(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'escape').symlink_to('/etc/passwd')
            with self.assertRaisesRegex(ValueError, 'contains a link'):
                release.inventory(root)


if __name__ == '__main__':
    unittest.main()
