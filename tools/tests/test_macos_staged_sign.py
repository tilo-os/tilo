"""Signing orchestration tests never invoke a key or execute an app."""
from contextlib import ExitStack
import json
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys
import tempfile
import types
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'dist/macos'))
import collection_staged_sign as s

IDENTITY = {'sha1': 'A' * 40, 'name': 'Developer ID Application: Example (TEAM123456)', 'team': 'TEAM123456'}


class StagedSigningTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='sign fixtures spaces & ')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.directory = self.root / 'unsigned'
        self.app = self.directory / 'Test & App.app'
        (self.app / 'Contents/MacOS').mkdir(parents=True)
        (self.app / 'Contents/Frameworks').mkdir()
        (self.app / 'Contents/Resources').mkdir()
        self.paths = ['Contents/Frameworks/libtest.dylib', 'Contents/MacOS/helper', 'Contents/MacOS/test.real']
        for relative in [*self.paths, 'Contents/MacOS/test']:
            (self.app / relative).write_bytes(b'not executable test data')
        (self.app / 'Contents/Info.plist').write_bytes(plistlib.dumps({'CFBundleExecutable': 'test', 'LSMinimumSystemVersion': '27.0'}))
        (self.app / 'Contents/Resources/asset').write_text('original resource')
        self.policy = self.directory / 'Test & App.entitlements'
        self.entitlements = {'com.apple.security.device.camera': True}
        self.policy.write_bytes(plistlib.dumps(self.entitlements))
        self.order = {'nested_code_in_order': self.paths, 'nested_process_entitlements': {
            'Contents/MacOS/test.real': self.policy.name}}
        self.app_plan = {'app': {'name': 'test', 'bundle': self.app.name}, 'source_root': str(self.root),
                         'runtime_plan': {}, 'source': 'b' * 40}
        self.facts = {'code_objects': 4, 'runtime': {p: {'slices': {'arm64': {}}} for p in self.paths},
                      'app': {'slices': {'arm64': {}}}}
        s.c.write_receipt(self.directory, self.app_plan, 'app-test')
        self.item = {'directory': str(self.directory), 'record': json.loads((self.directory / 'receipt.json').read_text()),
                     'receipt_sha256': s.c.digest(self.directory / 'receipt.json')}
        self.plan = {'source': 'b' * 40, 'manifest_sha256': 'c' * 64, 'apps': [self.item], 'held_apps': ['notes']}
        self.out = self.root / 'signed output'

    def test_inside_out_exact_policies_and_argument_quoting(self):
        calls = []
        def run(command, **kwargs):
            policy = Path(command[command.index('--entitlements') + 1])
            calls.append((command, plistlib.loads(policy.read_bytes()), kwargs['timeout']))
        with patch.object(s.native, 'run', side_effect=run), patch.object(s.runtime, 'floor'):
            s.sign_code(self.app, self.policy, {**self.order, 'identity': IDENTITY}, 120)
        self.assertEqual([call[0][-1] for call in calls], [self.app / p for p in self.paths] + [self.app])
        self.assertEqual([call[1] for call in calls], [{}, {}, self.entitlements, self.entitlements])
        for command, _, timeout in calls:
            self.assertEqual(command[:8], ['/usr/bin/codesign', '--force', '--sign', 'A' * 40, '--options', 'runtime', '--timestamp', '--entitlements'])
            self.assertNotIn('--deep', command)
            self.assertEqual(timeout, 120)

    def test_signing_failure_does_not_retry_or_sign_outer(self):
        with patch.object(s.native, 'run', side_effect=subprocess.TimeoutExpired('codesign', 120)) as run, patch.object(s.runtime, 'floor'):
            with self.assertRaises(subprocess.TimeoutExpired):
                s.sign_code(self.app, self.policy, {**self.order, 'identity': IDENTITY}, 120)
        self.assertEqual(run.call_count, 1)

    def test_exact_entitlements_verified_even_empty(self):
        with patch.object(s.native, 'run', return_value=plistlib.dumps(self.entitlements).decode()):
            with self.assertRaisesRegex(ValueError, 'entitlements differ'):
                s.verify_entitlements(self.app, {'Contents/MacOS/helper': {}})
        with patch.object(s.native, 'run', return_value=''):
            s.verify_entitlements(self.app, {'Contents/MacOS/helper': {}})
            with self.assertRaisesRegex(ValueError, 'entitlements differ'):
                s.verify_entitlements(self.app, {'Contents/MacOS/test.real': self.entitlements})

    def process_patches(self, stack):
        stack.enter_context(patch.object(s, 'unsigned', return_value=(self.app, self.policy, self.facts, self.order)))
        stack.enter_context(patch.object(s.shutil, 'disk_usage', return_value=types.SimpleNamespace(free=100 * 1024**3)))
        stack.enter_context(patch.object(s, 'current_inputs'))

    def test_failed_transaction_preserves_original_and_retry_resume_verify(self):
        original = s.c.inventory(self.directory)
        with ExitStack() as stack:
            self.process_patches(stack)
            sign = stack.enter_context(patch.object(s, 'sign_code', side_effect=ValueError('key denied')))
            verify = stack.enter_context(patch.object(s, 'verify_copy'))
            with self.assertRaisesRegex(ValueError, 'key denied'):
                s.process_app(self.out, self.plan, self.item, IDENTITY, False, 120)
            self.assertFalse((self.out / 'signed-test').exists())
            self.assertEqual(list(self.out.glob('.signed-test-*')), [])
            self.assertEqual(s.c.inventory(self.directory), original)
            sign.side_effect = None
            s.process_app(self.out, self.plan, self.item, IDENTITY, False, 120)
            self.assertTrue((self.out / 'signed-test/receipt.json').is_file())
            sign.reset_mock()
            verify.side_effect = ValueError('signature invalid on resume')
            with self.assertRaisesRegex(ValueError, 'signature invalid on resume'):
                s.process_app(self.out, self.plan, self.item, IDENTITY, True, 120)
            sign.assert_not_called()

    def test_hold_change_before_publish_discards_owned_copy(self):
        with ExitStack() as stack:
            self.process_patches(stack)
            stack.enter_context(patch.object(s, 'sign_code'))
            stack.enter_context(patch.object(s, 'verify_copy'))
            stack.enter_context(patch.object(s, 'current_inputs', side_effect=ValueError('hold changed')))
            with self.assertRaisesRegex(ValueError, 'hold changed'):
                s.process_app(self.out, self.plan, self.item, IDENTITY, False, 120)
        self.assertFalse((self.out / 'signed-test').exists())

    def test_source_copy_mismatch_stops_before_sign(self):
        real_copy = shutil.copy2
        def altered_copy(source, target, **kwargs):
            real_copy(source, target, **kwargs)
            if Path(target).suffix == '.entitlements':
                (Path(target).parent / self.app.name / 'Contents/Resources/asset').write_text('changed')
        with ExitStack() as stack:
            self.process_patches(stack)
            stack.enter_context(patch.object(s.shutil, 'copy2', side_effect=altered_copy))
            sign = stack.enter_context(patch.object(s, 'sign_code'))
            with self.assertRaisesRegex(ValueError, 'copy differs'):
                s.process_app(self.out, self.plan, self.item, IDENTITY, False, 120)
            sign.assert_not_called()

    def test_symlink_output_and_concurrent_run_rejected(self):
        link = self.root / 'linked'
        link.symlink_to(self.directory, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, 'symlink'):
            s.canonical(link / 'child')
        with s.run_lock(self.out):
            with self.assertRaisesRegex(ValueError, 'another staged signer'):
                with s.run_lock(self.out):
                    self.fail('second lock acquired')

    def test_signed_resources_and_projection_are_not_ignored(self):
        signed = self.root / 'copied.app'
        shutil.copytree(self.app, signed)
        (signed / 'Contents/_CodeSignature').mkdir()
        (signed / 'Contents/_CodeSignature/CodeResources').write_text('seal')
        with ExitStack() as stack:
            stack.enter_context(patch.object(s.native, 'inspect_bundle_metadata', return_value=(signed / 'Contents/MacOS/test', None, {'LSMinimumSystemVersion': '27.0'}, [signed / p for p in [*self.paths, 'Contents/MacOS/test']])))
            compare = stack.enter_context(patch.object(s, 'compare_signed_macho'))
            stack.enter_context(patch.object(s.runtime, 'inspect_payload', return_value=self.facts['runtime']))
            stack.enter_context(patch.object(s.runtime, 'image_info', return_value=self.facts['app']))
            signatures = stack.enter_context(patch.object(s.native, 'verify_signatures'))
            stack.enter_context(patch.object(s, 'verify_entitlements'))
            s.verify_copy(self.app, signed, self.app_plan, self.facts, self.policy, self.order, IDENTITY)
            self.assertEqual(compare.call_count, 4)
            signatures.assert_called_once()
            (signed / 'Contents/Resources/asset').write_text('changed')
            with self.assertRaisesRegex(ValueError, 'resource or layout changed'):
                s.verify_copy(self.app, signed, self.app_plan, self.facts, self.policy, self.order, IDENTITY)
            (signed / 'Contents/Resources/asset').write_text('original resource')
            compare.side_effect = ValueError('code mutation')
            with self.assertRaisesRegex(ValueError, 'code mutation'):
                s.verify_copy(self.app, signed, self.app_plan, self.facts, self.policy, self.order, IDENTITY)

    def test_selection_cannot_add_held_app_or_omit_duplicate_app(self):
        hold = self.root / 'holds.json'; hold.write_text(json.dumps({'apps': {'notes': {'reason': 'held'}}}))
        manifest = self.root / 'manifest.json'
        data = {'format': 1, 'source_root': str(self.root), 'source': 'b' * 40,
                'holds': {'path': str(hold), 'sha256': s.c.digest(hold)}, 'apps': []}
        catalog = {'apps': [{'name': 'test'}, {'name': 'notes'}], 'excluded': []}
        for names in [[], ['test', 'notes'], ['test', 'test']]:
            data['apps'] = [{'name': name} for name in names];manifest.write_text(json.dumps(data))
            with patch.object(s.apps, 'source_check'), patch.object(s.apps, 'source_catalog'), patch.object(s.c, 'app_catalog'), patch.object(s.c, 'make_plan', return_value=catalog):
                with self.assertRaisesRegex(ValueError, 'every eligible app exactly once'):
                    s.selection(manifest, s.c.digest(manifest))


if __name__ == '__main__':
    unittest.main()
