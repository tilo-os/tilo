import json
from pathlib import Path
import plistlib
from types import SimpleNamespace
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'dist/macos'))
import collection_local_apps as apps
from collection import digest, write_receipt


class LocalAppTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='local app spaces & $() ')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.app = self.root / 'SDK App.app'
        self.payload = self.root / 'payload'
        self.binary = self.file(self.app / 'Contents/MacOS/main', b'original app')
        self.helper = self.file(self.app / 'Contents/MacOS/tilo-runtime-mcp', b'original helper')
        self.info = {'CFBundleIdentifier': 'so.tilo.fixture', 'LSMinimumSystemVersion': '13.0',
                     'NSCameraUsageDescription': 'SDK generated description'}
        self.file(self.app / 'Contents/Info.plist', plistlib.dumps(self.info))
        self.file(self.payload / 'Contents/MacOS/tilo-runtime-mcp', b'launcher')
        self.file(self.payload / 'Contents/MacOS/tilo-runtime-mcp.real', b'relocated helper')
        self.file(self.payload / 'Contents/Resources/locale/de/LC_MESSAGES/glib20.mo', b'catalog')
        self.file(self.payload / 'receipt.json', b'{}')
        self.plan = {'payload': str(self.payload), 'runtime_plan': {'images': {
            'original helper': {'destination': 'Contents/MacOS/tilo-runtime-mcp.real', 'sha256': digest(self.helper)}}}}
        self.pin()

    def pin(self):
        write_receipt(self.payload, self.plan['runtime_plan'], 'local-runtime', {'images': {}})
        self.plan['payload_receipt_sha256'] = digest(self.payload / 'receipt.json')

    def file(self, path, data):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        return path

    def test_overlay_changes_only_private_minimum_and_helper(self):
        with patch.object(apps.runtime, 'floor'):
            apps.overlay(self.app, self.plan)
        info = plistlib.loads((self.app / 'Contents/Info.plist').read_bytes())
        self.assertEqual(info, {**self.info, 'LSMinimumSystemVersion': '27.0'})
        self.assertEqual(self.binary.read_bytes(), b'original app')
        self.assertEqual(self.helper.read_bytes(), b'launcher')
        self.assertEqual((self.app / 'Contents/MacOS/tilo-runtime-mcp.real').read_bytes(), b'relocated helper')
        self.assertFalse((self.app / 'receipt.json').exists())

    def test_wrong_original_helper_is_rejected_before_replacement(self):
        self.helper.write_bytes(b'foreign helper')
        with self.assertRaisesRegex(ValueError, 'qualified original'):
            apps.overlay(self.app, self.plan)
        self.assertEqual(self.helper.read_bytes(), b'foreign helper')

    def test_payload_cannot_overwrite_sdk_manifest_or_executable(self):
        self.file(self.payload / 'Contents/MacOS/main', b'foreign main')
        self.pin()
        with patch.object(apps.runtime, 'floor'), self.assertRaises(FileExistsError):
            apps.overlay(self.app, self.plan)
        self.assertEqual(self.binary.read_bytes(), b'original app')

    def test_payload_symlink_is_rejected(self):
        (self.payload / 'Contents/Resources/escape').symlink_to(self.binary)
        self.pin()
        with patch.object(apps.runtime, 'floor'), self.assertRaisesRegex(ValueError, 'symlink'):
            apps.overlay(self.app, self.plan)

    def test_payload_and_receipt_replacement_after_plan_is_rejected(self):
        (self.payload / 'Contents/MacOS/tilo-runtime-mcp').write_bytes(b'changed')
        write_receipt(self.payload, self.plan['runtime_plan'], 'local-runtime')
        with self.assertRaisesRegex(ValueError, 'SHA-256 mismatch'):
            apps.overlay(self.app, self.plan)
        self.assertEqual(self.helper.read_bytes(), b'original helper')

    def test_non_executable_input_is_rejected(self):
        self.binary.chmod(0o644)
        with self.assertRaisesRegex(ValueError, 'executable permission'):
            apps.executable(self.binary)

    def test_source_revision_and_tracked_changes_are_gates(self):
        revision = 'a' * 40
        with patch.object(apps, 'run', return_value='b' * 40):
            with self.assertRaisesRegex(ValueError, 'revision changed'):
                apps.source_check(self.root, revision)
        with patch.object(apps, 'run', side_effect=[revision, ' M apps/fixture/src/main.rs']):
            with self.assertRaisesRegex(ValueError, 'tracked changes'):
                apps.source_check(self.root, revision)

    def test_signing_inventory_orders_libraries_before_processes_and_app(self):
        facts = {'runtime': {
            'Contents/MacOS/tilo-runtime-mcp.real': {'slices': {'arm64': {'filetype': 'EXECUTE'}}},
            'Contents/Frameworks/a.dylib': {'slices': {'arm64': {'filetype': 'DYLIB'}}}}}
        result = apps.signing_inventory(self.app, self.app.with_suffix('.entitlements'), facts)
        self.assertEqual(result['nested_code_in_order'][0], 'Contents/Frameworks/a.dylib')
        self.assertEqual(result['final_bundle'], self.app.name)
        self.assertEqual(result['nested_process_entitlements'], {})
        self.assertEqual(result['notarization'], 'not authorized')

    def test_failed_sdk_leaves_no_published_app(self):
        plan = {**self.plan, 'source_root': str(self.root), 'source': 'a' * 40,
                'app': {'name': 'fixture'}, 'sdk': str(self.binary), 'sdk_sha256': digest(self.binary),
                'binary': str(self.binary)}
        out = self.root / 'output'
        with patch.object(apps, 'source_check'), patch.object(apps.runtime, 'floor'), \
                patch.object(apps.shutil, 'disk_usage', return_value=SimpleNamespace(free=100 * 1024**3)), \
                patch.object(apps, 'sdk_environment', return_value=({}, self.root / 'unused')), \
                patch.object(apps, 'bounded_build', side_effect=ValueError('SDK capability check failed')):
            with self.assertRaisesRegex(ValueError, 'SDK capability check'):
                apps.assemble(out, plan)
        self.assertFalse((out / 'app-fixture').exists())
        self.assertEqual(list(out.glob('.app-fixture-*')), [])

    def test_copy_admission_preserves_disk_floor_before_starting_sdk(self):
        plan = {**self.plan, 'source_root': str(self.root), 'source': 'a' * 40}
        out = self.root / 'output'
        with patch.object(apps, 'source_check'), patch.object(apps.runtime, 'floor'), \
                patch.object(apps.shutil, 'disk_usage', return_value=SimpleNamespace(free=20 * 1024**3)), \
                patch.object(apps, 'sdk_environment') as sdk:
            with self.assertRaisesRegex(ValueError, 'insufficient headroom'):
                apps.assemble(out, plan)
        sdk.assert_not_called()
        self.assertFalse(out.exists())


if __name__ == '__main__':
    unittest.main()
