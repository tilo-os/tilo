import importlib.util
import json
import os
from pathlib import Path
import platform
import shutil
import sys
import unittest
from unittest.mock import patch
import test_development_trust as fixtures

DEV = Path(__file__).resolve().parents[1] / 'dev'
sys.path.insert(0, str(DEV))
spec = importlib.util.spec_from_file_location('rebind', DEV / 'rebind-development-trust.py')
rebind = importlib.util.module_from_spec(spec)
spec.loader.exec_module(rebind)

def restart_fixture(test):
    """Give the next subTest phase of a merged test the fresh fixture it had
    as its own test: tear down, run cleanups, reset instance state, set up."""
    test.tearDown()
    test.doCleanups()
    fresh = type(test)(test._testMethodName).__dict__
    keep = {key: test.__dict__[key] for key in ("_outcome", "_subtest") if key in test.__dict__}
    test.__dict__.clear()
    test.__dict__.update(fresh, **keep)
    test.setUp()


@unittest.skipUnless(hasattr(os, 'getuid') and os.getuid() == 0
                     and shutil.which('cosign') and fixtures.trust.CRYPTO.is_file(),
                     'native root Rust-crypto fixture required')
class RebindTests(unittest.TestCase):
    setUpClass = classmethod(fixtures.PublicBundleTests.setUpClass.__func__)

    def setUp(self):
        fixtures.PublicBundleTests.setUp(self)
        self.image = Path(self.temporary.name) / 'image'
        self.old = self.output
        digest = fixtures.trust.validate(self.old)['manifest_sha256']
        self.target = 'registry.test/tilo-development/os:old'
        rebind.installer.install(self.old, self.old, self.image / 'usr/share/tilo',
            'development', digest, 'ab' * 32, self.target, self.image)
        for relative, data in [('usr/lib/tilo/generation', b'7\n'),
                               ('usr/lib/modules/test/vmlinuz', b'kernel bytes'),
                               ('usr/lib/modules/test/initramfs.img', b'old initramfs')]:
            path = self.image / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        (self.image / 'usr/lib/systemd/boot/efi').mkdir(parents=True)
        owner = Path(self.temporary.name) / 'new-owner'
        owner.write_text('cd' * 32 + '\n')
        self.new = Path(self.temporary.name) / 'new'
        fixtures.trust.create([*self.sources[:3], owner], self.new)
        self.arguments = dict(disposable=True, old_bundle=self.old, old_manifest=digest,
            old_target=self.target, new_bundle=self.new,
            new_manifest=fixtures.trust.validate(self.new)['manifest_sha256'],
            new_target='registry.test/tilo-development/os:new', new_owner='cd' * 32,
            architecture=platform.machine(), generation=7)

    def snapshot(self):
        return {str(path.relative_to(self.image)): path.read_bytes()
                for path in self.image.rglob('*') if path.is_file()}

    def test_replacement_lifecycle(self):
        with self.subTest(phase="replacement_preserves_kernel_generation_and_requires_initramfs"):
            before = self.snapshot()
            result = rebind.replace(self.image, **self.arguments)
            after = self.snapshot()
            marker = json.loads((self.image / rebind.hardware.DEVELOPMENT_PATH / 'initramfs-rebind-required.json').read_text())
            self.assertEqual(marker['purpose'], 'tilo-development-initramfs-rebind-required')
            self.assertEqual(marker['architecture'], platform.machine())
            self.assertEqual(marker['new_manifest_sha256'], self.arguments['new_manifest'])
            self.assertEqual(len(marker['original_kernel_sha256']), 64)
            self.assertEqual(len(marker['original_initramfs_sha256']), 64)
            self.assertTrue(result['initramfs_regeneration_required'])
            self.assertFalse(result['installable'])
            with self.assertRaisesRegex(ValueError, 'verified initramfs regeneration'):
                rebind.retention.source.mounted_identity(self.image, self.arguments['new_target'],
                    self.arguments['new_manifest'], platform.machine())
            with self.assertRaisesRegex(ValueError, 'verified initramfs regeneration'):
                rebind.retention.rootfs.source.mounted_identity(self.image, self.arguments['new_target'],
                    self.arguments['new_manifest'], platform.machine())
            for name in before:
                if name.startswith('usr/lib/'):
                    self.assertEqual(before[name], after[name])
            self.assertEqual(rebind.hardware.development_origin(self.image), self.arguments['new_target'])
            self.assertEqual(after['usr/share/tilo/trust/tilo-main-registry-owner.pub'], b'cd' * 32 + b'\n')
        restart_fixture(self)
        with self.subTest(phase="interrupted_replacement_leaves_acceptance_blocking_marker"):
            # Staging the replacement uses os.replace too (install-public-trust
            # writes atomically); interrupt only the writes into the image.
            real_replace = os.replace

            def interrupted(source, destination, *args, **kwargs):
                if Path(destination).is_relative_to(self.image):
                    raise OSError('fixture interruption')
                return real_replace(source, destination, *args, **kwargs)

            with patch.object(rebind.os, 'replace', side_effect=interrupted):
                with self.assertRaises(OSError):
                    rebind.replace(self.image, **self.arguments)
            with self.assertRaisesRegex(ValueError, 'verified initramfs regeneration'):
                rebind.retention.source.mounted_identity(self.image, self.target,
                    self.arguments['old_manifest'], platform.machine())
        restart_fixture(self)
        with self.subTest(phase="profile_target_changes_but_selection_is_preserved"):
            definition = dict(schema_version=1, id='fixture', architecture=platform.machine(), devicetree=None, firmware={})
            profile = dict(schema_version=1, id='fixture', architecture=platform.machine(), devicetree=None,
                           devicetree_sha256=None, target_update_ref=self.target)
            (self.image / rebind.hardware.DEFINITION_PATH).write_text(json.dumps(definition))
            (self.image / rebind.hardware.PROFILE_PATH).write_text(json.dumps(profile))
            rebind.replace(self.image, **self.arguments)
            self.assertEqual(rebind.hardware.read_profile(self.image), profile | {'target_update_ref': self.arguments['new_target']})
            self.assertEqual(json.loads((self.image / rebind.hardware.DEFINITION_PATH).read_text()), definition)

    def test_invalid_inputs_leave_state_unchanged(self):
        with self.subTest(phase="invalid_independent_inputs_leave_all_old_bytes_unchanged"):
            for field, value in [('old_manifest', '0' * 64), ('new_manifest', '0' * 64),
                                 ('old_target', 'registry.test/tilo-development/os:wrong'),
                                 ('new_target', 'registry.tilo.so/tilo/tilo:latest'),
                                 ('new_owner', 'ef' * 32), ('generation', 8), ('disposable', False),
                                 ('architecture', 'wrong')]:
                before = self.snapshot()
                with self.subTest(field=field), self.assertRaises(ValueError):
                    rebind.replace(self.image, **(self.arguments | {field: value}))
                self.assertEqual(before, self.snapshot())
        restart_fixture(self)
        with self.subTest(phase="completed_and_redirected_destinations_fail_before_writes"):
            for relative in ('boot/EFI/Linux', 'kernel', 'usr/lib/systemd/boot/efi/loader.efi.signed'):
                path = self.image / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b'completed')
                before = self.snapshot()
                with self.assertRaises(ValueError):
                    rebind.replace(self.image, **self.arguments)
                self.assertEqual(before, self.snapshot())
                path.unlink()
            key = self.image / 'usr/share/tilo/cosign.pub'
            original = key.read_bytes()
            key.unlink()
            key.symlink_to(self.old / 'cosign.pub')
            with self.assertRaises(ValueError):
                rebind.replace(self.image, **self.arguments)
            self.assertEqual((self.old / 'cosign.pub').read_bytes(), original)
        restart_fixture(self)
        with self.subTest(phase="invalid_old_policy_or_bundle_redirect_leaves_old_state_unchanged"):
            policy = self.image / 'etc/containers/policy.json'
            original = policy.read_bytes()
            policy.write_text('{"default":[{"type":"insecureAcceptAnything"}]}')
            before = self.snapshot()
            with self.assertRaises(ValueError):
                rebind.replace(self.image, **self.arguments)
            self.assertEqual(before, self.snapshot())
            policy.write_bytes(original)
            key = self.old / 'cosign.pub'
            key.unlink()
            key.symlink_to(self.sources[0])
            before = self.snapshot()
            with self.assertRaises(ValueError):
                rebind.replace(self.image, **self.arguments)
            self.assertEqual(before, self.snapshot())
        restart_fixture(self)
        with self.subTest(phase="redirected_bundle_root_and_unexpected_file_reject_before_copy_or_writes"):
            for field in ('old_bundle', 'new_bundle'):
                linked = Path(self.temporary.name) / ('linked-' + field)
                linked.symlink_to(self.arguments[field], target_is_directory=True)
                before = self.snapshot()
                with self.subTest(field=field), self.assertRaises(ValueError):
                    rebind.replace(self.image, **(self.arguments | {field: linked}))
                self.assertEqual(before, self.snapshot())
            unexpected = self.new / 'unexpected'
            unexpected.mkdir()
            # A nested unexpected tree must be rejected without recursively copying
            # or reading its contents into private staging.
            (unexpected / 'payload').write_bytes(b'unexpected data')
            before = self.snapshot()
            original_read = rebind.installer.trust.read_public
            def bounded_read(path):
                self.assertNotIn('unexpected', Path(path).parts)
                return original_read(path)
            with patch.object(rebind.installer.trust, 'read_public', side_effect=bounded_read):
                with self.assertRaises(ValueError):
                    rebind.replace(self.image, **self.arguments)
            self.assertEqual(before, self.snapshot())

if __name__ == '__main__':
    unittest.main()
