import copy
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import time
import unittest
from types import SimpleNamespace
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'dev'))
import signing_spool as spool
SPEC = importlib.util.spec_from_file_location('finalizer', Path(__file__).resolve().parents[1] / 'dev/finalize-development-rootfs.py')
finalizer = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(finalizer)


def request():
    return {'schema': 1, 'source_revision': 'a' * 40, 'source_dir': '/verified/source', 'helpers_revision': 'b' * 40,
            'receipt': '/verified/receipt.json', 'receipt_sha256': 'c' * 64,
            'public_bundle': '/verified/original', 'manifest_sha256': 'd' * 64,
            'target': 'code.sandbox.example/qos/tilo-development:old-aarch64',
            'new_public_bundle': '/verified/new', 'new_manifest_sha256': 'e' * 64,
            'new_target': 'code.sandbox.example/qos/tilo-development:new-aarch64', 'new_owner': 'f' * 64,
            'architecture': 'aarch64', 'generation': 7, 'source_date_epoch': 1234,
            'base_ref': 'registry.invalid/base@sha256:' + '1' * 64,
            'rpms_ref': 'registry.invalid/rpms@sha256:' + '2' * 64,
            'kernel_ref': 'registry.invalid/kernel@sha256:' + '3' * 64,
            'wpe_ref': 'registry.invalid/wpe@sha256:' + '4' * 64,
            'signer': '/var/lib/bridge/signer', 'systemd_version': '259.1',
            'minimum_free_bytes': 1024**3, 'stage_timeout_seconds': 600,
            'publish_repository': 'code.sandbox.example/qos/tilo-development'}


class FinalizationTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(dir='/var/lib', prefix='tilo-finalization-tests-')
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)

    def driver(self):
        value = finalizer.Driver(request(), Path('/verified/helpers'), self.root)
        value.capacity = lambda: None
        value.inspect = lambda identifier: finalizer.image_id(identifier)
        return value

    def test_media_profile_explicit_and_bad_profile_rejected(self):
        value=request();value['boot_profile']='installer-media'
        self.assertEqual(finalizer.validate_request(value)['boot_profile'],'installer-media')
        value['boot_profile']='unchecked-image'
        with self.assertRaises(ValueError):finalizer.validate_request(value)

    def test_final_build_reuses_signed_rootfs_and_never_calls_build_image(self):
        identifier = 'sha256:' + '9' * 64
        command = finalizer.build_final_command(request(), identifier, self.root / 'image.iid')
        self.assertIn('rootfs=container-image://' + identifier, command)
        self.assertEqual(command[command.index('--target') + 1], 'final')
        self.assertIn('--skip-unused-stages=true', command)
        self.assertIn('--pull=never', command)
        self.assertIn('--network=none', command)
        self.assertNotIn('build-image.sh', ' '.join(command))
        self.assertNotIn('cargo', ' '.join(command))
        with self.assertRaises(ValueError):
            finalizer.build_final_command(request(), 'mutable:tag', self.root / 'image.iid')

    def test_completed_stage_resumes_without_signing_again(self):
        driver = self.driver()
        expected = 'sha256:' + '9' * 64
        first = driver.stage(1, lambda: {'image_id': expected})
        resumed = self.driver()
        calls = []
        result = resumed.stage(1, lambda: calls.append('must not run'))
        self.assertEqual(result, first)
        self.assertEqual(calls, [])

    def test_interrupted_stage_cannot_replay_signing(self):
        driver = self.driver()
        def interrupted():
            raise ValueError('fixture interruption')
        with self.assertRaises(ValueError):
            driver.stage(1, interrupted)
        self.assertFalse((self.root / '01-rebind.json').exists())
        with self.assertRaisesRegex(ValueError, 'previously started'):
            self.driver().stage(1, lambda: self.fail('must not retry'))

    def test_changed_request_and_checkpoint_chain_rejected(self):
        driver = self.driver()
        driver.stage(1, lambda: {'image_id': 'sha256:' + '9' * 64})
        changed = self.driver()
        changed.binding = '0' * 64
        with self.assertRaisesRegex(ValueError, 'another input chain'):
            changed.stage(1, lambda: self.fail('must not run'))
        path = self.root / '01-rebind.json'
        value = spool.document(path)
        value['parent_receipt_sha256'] = '1' * 64
        path.write_bytes(spool.canonical(value))
        with self.assertRaises(ValueError):
            self.driver().stage(1, lambda: self.fail('must not run'))

    def test_registry_exact_manifest_and_config_identity(self):
        config = spool.canonical({'os': 'linux', 'architecture': 'arm64', 'config': {'Labels': {'org.opencontainers.image.revision': 'a' * 40}}})
        config_id = 'sha256:' + spool.digest(config)
        manifest = spool.canonical({'schemaVersion': 2, 'config': {'digest': config_id}})
        manifest_id = 'sha256:' + spool.digest(manifest)
        finalizer.verify_registry_bytes(manifest, config, manifest_id, config_id, 'aarch64', 'a' * 40)
        for args in ((manifest + b'\n', config, manifest_id, config_id, 'aarch64', 'a' * 40),
                     (manifest, config + b'\n', manifest_id, config_id, 'aarch64', 'a' * 40),
                     (manifest, config, manifest_id, config_id, 'x86_64', 'a' * 40)):
            with self.assertRaises(ValueError):
                finalizer.verify_registry_bytes(*args)

    def test_no_cross_repository_or_mutable_build_inputs(self):
        for change in ({'publish_repository': 'code.sandbox.example/qos/tilo-development-other'},
                       {'base_ref': 'registry.invalid/base:latest'}, {'stage_timeout_seconds': 99999}):
            with self.subTest(change=change), self.assertRaises(ValueError):
                finalizer.validate_request(request() | change)

    def test_full_stage_wiring_retains_published_artifact_before_success(self):
        q = request()
        source = self.root / 'source'
        (source / 'os').mkdir(parents=True)
        (source / 'os/Containerfile').write_text('fixture delegated recipe')
        q['source_dir'] = str(source)
        config = spool.canonical({'os': 'linux', 'architecture': 'arm64', 'config': {'Labels': {'org.opencontainers.image.revision': q['source_revision']}}})
        final_id = 'sha256:' + spool.digest(config)
        manifest = spool.canonical({'schemaVersion': 2, 'config': {'digest': final_id}})
        manifest_id = 'sha256:' + spool.digest(manifest)
        calls = []
        driver = finalizer.Driver(q, '/verified/helpers', self.root)
        driver.preflight = lambda: None
        driver.capacity = lambda: None
        driver.inspect = lambda identifier: finalizer.image_id(identifier)
        def helper(name, arguments):
            calls.append(name)
            if name == 'rebind-development-rootfs.py':
                return {'rootfs_id': 'sha256:' + '1' * 64}
            if name == 'sign-development-rootfs.py':
                return {'signed_rootfs_id': 'sha256:' + '2' * 64}
            if name == 'compute-development-composefs.py':
                return {'composefs_digest': 'a' * 128}
            self.fail('unknown delegated helper')
        driver.helper = helper
        driver.module = lambda *args: SimpleNamespace(validate=lambda *args: {'identities': {'efi-signing.crt': 'f' * 64}})
        def command(arguments):
            argv = [str(x) for x in arguments]
            calls.append(argv)
            if '--iidfile' in argv:
                Path(argv[argv.index('--iidfile') + 1]).write_text('sha256:' + '3' * 64)
            if 'inspect' in argv and argv[0] == 'podman':
                return spool.canonical([{'Id': final_id}])
            if 'verify-finalized-development-image.py' in ' '.join(argv):
                return spool.canonical({'installable': False, 'fixture': 'delegated verification mocked'})
            if 'push' in argv:
                Path(argv[argv.index('--digestfile') + 1]).write_text(manifest_id)
            if argv[0] == 'skopeo':
                return config if '--config' in argv else manifest
            return b''
        driver.command = command
        result = driver.execute()
        self.assertTrue(result['published'])
        self.assertEqual(result['published_reference'], q['publish_repository'] + '@' + manifest_id)
        self.assertFalse(result['installable'])
        self.assertTrue(result['cosign_signing_required'])
        self.assertTrue((self.root / '08-published.json').is_file())
        pushes = [c for c in calls if isinstance(c, list) and 'push' in c]
        self.assertEqual(len(pushes), 1)
        self.assertNotEqual(pushes[0][-1], 'docker://' + q['new_target'])

    def test_timeout_kills_descendant_after_leader_exits(self):
        pidfile = self.root / 'descendant.pid'
        script = 'import subprocess,sys; p=subprocess.Popen([sys.executable,"-c","import time;time.sleep(60)"]);open(sys.argv[1],"w").write(str(p.pid))'
        with self.assertRaisesRegex(ValueError, 'deadline'):
            finalizer.run_bounded([sys.executable, '-c', script, str(pidfile)], dict(os.environ), 0.2)
        pid = int(pidfile.read_text())
        deadline = time.monotonic() + 2
        while time.monotonic() < deadline:
            path = Path(f'/proc/{pid}/stat')
            if not path.exists() or path.read_text().split()[2] == 'Z':
                break
            time.sleep(0.01)
        else:
            self.fail('descendant retained stdout after deadline')

if __name__ == '__main__':
    unittest.main()
