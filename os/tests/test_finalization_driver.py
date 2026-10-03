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
            'target': 'registry.tilo.so/tilo/tilo-development:old-aarch64',
            'new_public_bundle': '/verified/new', 'new_manifest_sha256': 'e' * 64,
            'new_target': 'registry.tilo.so/tilo/tilo-development:new-aarch64', 'new_owner': 'f' * 64,
            'architecture': 'aarch64', 'generation': 7, 'source_date_epoch': 1234,
            'base_ref': 'registry.invalid/base@sha256:' + '1' * 64,
            'rpms_ref': 'registry.invalid/rpms@sha256:' + '2' * 64,
            'kernel_ref': 'registry.invalid/kernel@sha256:' + '3' * 64,
            'wpe_ref': 'registry.invalid/wpe@sha256:' + '4' * 64,
            'signer': '/var/lib/bridge/signer', 'systemd_version': '259.1',
            'minimum_free_bytes': 1024**3, 'stage_timeout_seconds': 600,
            'signing_response_timeout_seconds': 1800, 'prediction_stage_timeout_seconds': 3600,
            'publish_repository': 'registry.tilo.so/tilo/tilo-development'}


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

    def test_final_build_reuse_and_resume(self):
        with self.subTest(phase="media_profile_explicit_and_bad_profile_rejected"):
            value=request();value['boot_profile']='installer-media'
            self.assertEqual(finalizer.validate_request(value)['boot_profile'],'installer-media')
            value['boot_profile']='unchecked-image'
            with self.assertRaises(ValueError):finalizer.validate_request(value)
        restart_fixture(self)
        with self.subTest(phase="final_build_reuses_signed_rootfs_and_never_calls_build_image"):
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
        restart_fixture(self)
        with self.subTest(phase="completed_stage_resumes_without_signing_again"):
            driver = self.driver()
            expected = 'sha256:' + '9' * 64
            first = driver.stage(1, lambda: {'image_id': expected})
            resumed = self.driver()
            calls = []
            result = resumed.stage(1, lambda: calls.append('must not run'))
            self.assertEqual(result, first)
            self.assertEqual(calls, [])
        restart_fixture(self)
        with self.subTest(phase="interrupted_stage_cannot_replay_signing"):
            driver = self.driver()
            def interrupted():
                raise ValueError('fixture interruption')
            with self.assertRaises(ValueError):
                driver.stage(1, interrupted)
            self.assertFalse((self.root / '01-rebind.json').exists())
            with self.assertRaisesRegex(ValueError, 'previously started'):
                self.driver().stage(1, lambda: self.fail('must not retry'))
        restart_fixture(self)
        with self.subTest(phase="changed_request_and_checkpoint_chain_rejected"):
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

    def test_checkpoint_publication_and_restore(self):
        with self.subTest(phase="registry_reported_manifest_and_config_descriptor_identity"):
            config = spool.canonical({'os': 'linux', 'architecture': 'arm64', 'config': {'Labels': {'org.opencontainers.image.revision': 'a' * 40}}})
            config_id = 'sha256:' + spool.digest(config)
            manifest = spool.canonical({'schemaVersion': 2, 'config': {'digest': config_id}})
            manifest_id = 'sha256:' + spool.digest(manifest)
            # Skopeo reserializes ``inspect --config`` output. Its bytes need not
            # hash to the raw blob digest carried by the immutable manifest.
            inspected_config = json.dumps(json.loads(config), indent=2).encode() + b'\n'
            self.assertNotEqual('sha256:' + spool.digest(inspected_config), config_id)
            finalizer.verify_registry_bytes(manifest + b'\n', inspected_config, manifest_id, manifest_id,
                                            config_id, 'aarch64', 'a' * 40)
            wrong_descriptor = spool.canonical({'schemaVersion': 2, 'config': {'digest': 'sha256:' + '0' * 64}})
            wrong_architecture = spool.canonical({'os': 'linux', 'architecture': 'amd64',
                'config': {'Labels': {'org.opencontainers.image.revision': 'a' * 40}}})
            for args in ((manifest, inspected_config, 'sha256:' + '9' * 64, manifest_id,
                          config_id, 'aarch64', 'a' * 40),
                         (wrong_descriptor, inspected_config, 'sha256:' + spool.digest(wrong_descriptor),
                          'sha256:' + spool.digest(wrong_descriptor), config_id, 'aarch64', 'a' * 40),
                         (manifest, wrong_architecture, manifest_id, manifest_id, config_id, 'aarch64', 'a' * 40),
                         (manifest, inspected_config, manifest_id, manifest_id, config_id, 'x86_64', 'a' * 40)):
                with self.assertRaises(ValueError):
                    finalizer.verify_registry_bytes(*args)
        restart_fixture(self)
        with self.subTest(phase="stage5_checkpoint_publishes_and_verifies_immutable_registry_bytes"):
            q = request()
            driver = self.driver()
            config = spool.canonical({'os': 'linux', 'architecture': 'arm64',
                'config': {'Labels': {'org.opencontainers.image.revision': q['source_revision']}}})
            image = 'sha256:' + spool.digest(config)
            manifest = spool.canonical({'schemaVersion': 2, 'config': {'digest': image}})
            digest = 'sha256:' + spool.digest(manifest)
            calls = []
            driver.current_context = lambda: {'sha256': '7' * 64, 'revision': q['source_revision'],
                'architecture': q['architecture'], 'manifest_sha256': q['new_manifest_sha256'],
                'target': q['new_target']}
            driver.registry_bytes = lambda reference: (manifest, config, digest)
            def command(arguments):
                calls.append([str(value) for value in arguments])
                Path(arguments[arguments.index('--digestfile') + 1]).write_text(digest)
                return b''
            driver.command = command
            result = driver.publish_checkpoint(image, 5)
            self.assertEqual(result['checkpoint_reference'], q['publish_repository'] + '@' + digest)
            self.assertEqual(result['checkpoint_config_digest'], image)
            self.assertEqual(result['checkpoint_stage'], 5)
            self.assertEqual(result['checkpoint_binding'], finalizer.checkpoint_binding(q, driver.binding))
            self.assertEqual(result['producer_context_sha256'], '7' * 64)
            self.assertEqual(calls[0][-2:], [image, 'docker://' + q['publish_repository'] +
                ':finalization-' + driver.binding + '-stage5'])
        restart_fixture(self)
        with self.subTest(phase="checkpoint_context_binds_stable_fields_but_allows_a_new_attempt_identity"):
            q = request()
            bridge = self.root / 'bridge'
            bridge.mkdir(mode=0o700)
            q['signer'] = str(bridge / 'signer')
            context = {'schema': 1, 'job_id': 'new-job', 'pod_uid': 'new-pod', 'container_id': 'new-container',
                'image_id': 'sha256:' + '1' * 64, 'revision': q['source_revision'],
                'architecture': q['architecture'], 'manifest_sha256': q['new_manifest_sha256'],
                'target': q['new_target']}
            (bridge / 'bridge.json').write_bytes(spool.canonical({'context': context}))
            value = finalizer.Driver(q, '/verified/helpers', self.root).current_context()
            self.assertEqual(value['sha256'], spool.context(context))
            context['target'] = 'registry.tilo.so/tilo/tilo-development:other-aarch64'
            (bridge / 'bridge.json').write_bytes(spool.canonical({'context': context}))
            with self.assertRaisesRegex(ValueError, 'current signing context differs'):
                finalizer.Driver(q, '/verified/helpers', self.root).current_context()
        restart_fixture(self)
        with self.subTest(phase="stage5_checkpoint_restores_across_contexts_without_replaying_prior_stages"):
            q = request()
            config = spool.canonical({'os': 'linux', 'architecture': 'arm64',
                'config': {'Labels': {'org.opencontainers.image.revision': q['source_revision']}}})
            image = 'sha256:' + spool.digest(config)
            manifest = spool.canonical({'schemaVersion': 2, 'config': {'digest': image}})
            digest = 'sha256:' + spool.digest(manifest)
            producer = {'sha256': '7' * 64, 'revision': q['source_revision'], 'architecture': q['architecture'],
                        'manifest_sha256': q['new_manifest_sha256'], 'target': q['new_target']}
            writer = self.driver()
            for number in range(1, 5):
                writer.stage(number, lambda number=number: {'image_id': 'sha256:' + str(number) * 64})
            writer.stage(5, lambda: {'image_id': image, 'composefs_digest': 'a' * 128,
                'checkpoint_reference': q['publish_repository'] + '@' + digest,
                'checkpoint_manifest_digest': digest, 'checkpoint_config_digest': image,
                'checkpoint_binding': finalizer.checkpoint_binding(q, writer.binding),
                'checkpoint_stage': 5,
                'producer_context_sha256': producer['sha256']})
            restored = self.driver()
            restored.current_context = lambda: {**producer, 'sha256': '8' * 64}
            restored.registry_bytes = lambda reference: (manifest, config, digest)
            commands, inspected = [], []
            restored.command = lambda arguments: commands.append([str(value) for value in arguments]) or b''
            restored.inspect = lambda identifier: inspected.append(identifier)
            self.assertTrue(restored.restore_checkpoint())
            for number in range(1, 6):
                restored.stage(number, lambda: self.fail('completed stage must not replay'))
            self.assertEqual(inspected, [image, image])
            self.assertEqual(commands, [['podman', '--remote=false', 'pull', '--tls-verify=true',
                                         q['publish_repository'] + '@' + digest]])
        restart_fixture(self)
        with self.subTest(phase="stage2_checkpoint_restores_loader_without_rebinding_or_signing"):
            q = request()
            config = spool.canonical({'os': 'linux', 'architecture': 'arm64',
                'config': {'Labels': {'org.opencontainers.image.revision': q['source_revision']}}})
            image = 'sha256:' + spool.digest(config)
            manifest = spool.canonical({'schemaVersion': 2, 'config': {'digest': image}})
            digest = 'sha256:' + spool.digest(manifest)
            context = {'sha256': '7' * 64, 'revision': q['source_revision'],
                'architecture': q['architecture'], 'manifest_sha256': q['new_manifest_sha256'],
                'target': q['new_target']}
            writer = self.driver()
            writer.stage(1, lambda: {'image_id': 'sha256:' + '1' * 64})
            writer.stage(2, lambda: {'image_id': image,
                'checkpoint_reference': q['publish_repository'] + '@' + digest,
                'checkpoint_manifest_digest': digest, 'checkpoint_config_digest': image,
                'checkpoint_binding': finalizer.checkpoint_binding(q, writer.binding),
                'checkpoint_stage': 2, 'producer_context_sha256': context['sha256']})
            restored = self.driver()
            restored.current_context = lambda: {**context, 'sha256': '8' * 64}
            restored.registry_bytes = lambda reference: (manifest, config, digest)
            commands, inspected = [], []
            restored.command = lambda arguments: commands.append([str(value) for value in arguments]) or b''
            restored.inspect = lambda identifier: inspected.append(identifier)
            self.assertTrue(restored.restore_checkpoint())
            for number in (1, 2):
                restored.stage(number, lambda: self.fail('rebind/loader must not replay'))
            self.assertEqual(restored.restored_stage, 2)
            self.assertEqual(inspected, [image, image])
            self.assertEqual(commands, [['podman', '--remote=false', 'pull', '--tls-verify=true',
                                         q['publish_repository'] + '@' + digest]])
        restart_fixture(self)
        with self.subTest(phase="stage5_restore_rejects_changed_stable_or_registry_identity_before_pull"):
            q = request()
            driver = self.driver()
            for number in range(1, 5):
                driver.stage(number, lambda number=number: {'image_id': 'sha256:' + str(number) * 64})
            image, digest = 'sha256:' + '5' * 64, 'sha256:' + '6' * 64
            context = {'sha256': '7' * 64, 'revision': q['source_revision'], 'architecture': q['architecture'],
                       'manifest_sha256': q['new_manifest_sha256'], 'target': q['new_target']}
            driver.stage(5, lambda: {'image_id': image, 'composefs_digest': 'a' * 128,
                'checkpoint_reference': q['publish_repository'] + '@' + digest,
                'checkpoint_manifest_digest': digest, 'checkpoint_config_digest': image,
                'checkpoint_binding': finalizer.checkpoint_binding(q, driver.binding),
                'checkpoint_stage': 5,
                'producer_context_sha256': context['sha256']})
            receipt = spool.document(self.root / '05-reseal.json')
            receipt['checkpoint_binding']['helpers_revision'] = '0' * 40
            (self.root / '05-reseal.json').write_bytes(spool.canonical(receipt))
            resumed = self.driver()
            resumed.current_context = lambda: {**context, 'sha256': '8' * 64}
            resumed.command = lambda arguments: self.fail('untrusted checkpoint must not touch registry')
            with self.assertRaisesRegex(ValueError, 'stable binding'):
                resumed.restore_checkpoint()

    def test_target_origin_contract(self):
        with self.subTest(phase="no_cross_repository_or_mutable_build_inputs"):
            for change in ({'publish_repository': 'registry.tilo.so/tilo/tilo-development-other'},
                           {'base_ref': 'registry.invalid/base:latest'}, {'stage_timeout_seconds': 99999}):
                with self.subTest(change=change), self.assertRaises(ValueError):
                    finalizer.validate_request(request() | change)
        restart_fixture(self)
        with self.subTest(phase="target_origins_require_the_matching_native_suffix"):
            repository = 'registry.tilo.so/tilo/tilo-development'
            for field in ('target', 'new_target'):
                for suffix in ('installed-ca33fae89a24-aarch64-9787', 'latest', 'next-x86_64'):
                    with self.subTest(field=field, suffix=suffix), self.assertRaisesRegex(
                            ValueError, f'invalid {field}:.*matching native architecture'):
                        finalizer.validate_request(request() | {field: repository + ':' + suffix})
        restart_fixture(self)
        with self.subTest(phase="native_tags_and_retained_digests_use_shared_origin_contract"):
            repository = 'registry.tilo.so/tilo/tilo-development'
            for architecture in ('aarch64', 'x86_64'):
                new_target = repository + ':installed-ca33fae89a24-9787-' + architecture
                for target in (repository + ':' + architecture,
                               repository + ':old-' + architecture,
                               repository + '@sha256:' + 'a' * 64,
                               'registry.vm.test:5443/tilo-development/os:previous'):
                    value = request() | {'architecture': architecture, 'target': target,
                                         'new_target': new_target}
                    with self.subTest(architecture=architecture, target=target):
                        self.assertEqual(finalizer.validate_request(value), value)
        restart_fixture(self)
        with self.subTest(phase="target_origin_rejects_aliases_and_invalid_digest_without_widening_publication"):
            repository = 'registry.tilo.so/tilo/tilo-development'
            for target in ('alias.example/tilo/tilo-development:old-aarch64',
                           'registry.tilo.so/tilo/tilo:old-aarch64',
                           repository + '@sha256:' + 'A' * 64,
                           repository + '@sha256:' + 'a' * 63, None):
                with self.subTest(target=target), self.assertRaises(ValueError):
                    finalizer.validate_request(request() | {'target': target})
            # Publishing binds a new tag; accepting retained digest inputs must not
            # silently widen that separate output contract.
            with self.assertRaisesRegex(ValueError, 'publication must use'):
                finalizer.validate_request(request() | {'new_target': repository + '@sha256:' + 'a' * 64})

    def test_deadlines_and_diagnostics(self):
        with self.subTest(phase="prediction_deadline_exceeds_signing_and_non_signing_budgets"):
            for change in ({'signing_response_timeout_seconds': 1801},
                           {'prediction_stage_timeout_seconds': 8401},
                           {'prediction_stage_timeout_seconds': 2400},
                           {'prediction_stage_timeout_seconds': 2399}):
                with self.subTest(change=change), self.assertRaises(ValueError):
                    finalizer.validate_request(request() | change)
            self.assertEqual(finalizer.validate_request(request() | {
                'prediction_stage_timeout_seconds': 2401})['prediction_stage_timeout_seconds'], 2401)
        restart_fixture(self)
        with self.subTest(phase="timeout_kills_descendant_after_leader_exits"):
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
        restart_fixture(self)
        with self.subTest(phase="failed_command_writes_bounded_sanitized_diagnostics"):
            q = request()
            driver = finalizer.Driver(q, '/verified/helpers', self.root)
            driver.status({'stage': 'provisional', 'state': 'running', 'automatic_retry': False})
            script = ('import sys;print("useful failure");print("TOKEN=do-not-publish");'
                      'print("https://operator:private@example.invalid/path");sys.exit(7)')
            with self.assertRaisesRegex(finalizer.CommandFailure, 'checkpoint not accepted'):
                driver.command([sys.executable, '-c', script])
            receipt = spool.document(self.root / 'failure.json')
            self.assertEqual(receipt['stage'], 'provisional')
            self.assertEqual(receipt['exit_code'], 7)
            self.assertEqual(receipt['executable'], Path(sys.executable).name)
            self.assertEqual(receipt['argument_count'], 3)
            self.assertRegex(receipt['arguments_sha256'], r'^[0-9a-f]{64}$')
            self.assertRegex(receipt['output_sha256'], r'^[0-9a-f]{64}$')
            self.assertIn('useful failure', receipt['output_tail'])
            self.assertNotIn('do-not-publish', receipt['output_tail'])
            self.assertNotIn('private', receipt['output_tail'])
            self.assertIn('https://***:***@example.invalid/path', receipt['output_tail'])
            self.assertLessEqual(len(receipt['output_tail']), 4096)
        restart_fixture(self)
        with self.subTest(phase="full_stage_wiring_retains_published_artifact_before_success"):
            q = request()
            source = self.root / 'source'
            (source / 'os').mkdir(parents=True)
            (source / 'os/Containerfile').write_text('fixture delegated recipe')
            q['source_dir'] = str(source)
            config = spool.canonical({'os': 'linux', 'architecture': 'arm64', 'config': {'Labels': {'org.opencontainers.image.revision': q['source_revision']}}})
            final_id = 'sha256:' + spool.digest(config)
            manifest = spool.canonical({'schemaVersion': 2, 'config': {'digest': final_id}})
            manifest_id = 'sha256:' + spool.digest(manifest)
            loader_config = spool.canonical({'os': 'linux', 'architecture': 'arm64',
                'config': {'Labels': {'org.opencontainers.image.revision': q['source_revision'],
                                      'fixture.stage': 'loader'}}})
            loader_id = 'sha256:' + spool.digest(loader_config)
            registry = {}
            calls, timed_calls = [], []
            driver = finalizer.Driver(q, '/verified/helpers', self.root)
            driver.preflight = lambda: None
            driver.capacity = lambda: None
            driver.inspect = lambda identifier: finalizer.image_id(identifier)
            def helper(name, arguments):
                calls.append(name)
                if name == 'rebind-development-rootfs.py':
                    return {'rootfs_id': 'sha256:' + '1' * 64}
                if name == 'sign-development-rootfs.py':
                    return {'signed_rootfs_id': loader_id}
                if name == 'compute-development-composefs.py':
                    return {'composefs_digest': 'a' * 128}
                self.fail('unknown delegated helper')
            driver.helper = helper
            driver.module = lambda *args: SimpleNamespace(validate=lambda *args: {'identities': {'efi-signing.crt': 'f' * 64}})
            driver.current_context = lambda: {'sha256': '7' * 64, 'revision': q['source_revision'],
                'architecture': q['architecture'], 'manifest_sha256': q['new_manifest_sha256'],
                'target': q['new_target']}
            def command(arguments, timeout=None):
                argv = [str(x) for x in arguments]
                calls.append(argv)
                if timeout is not None:
                    timed_calls.append((argv, timeout))
                if '--iidfile' in argv:
                    Path(argv[argv.index('--iidfile') + 1]).write_text(final_id)
                if 'inspect' in argv and argv[0] == 'podman':
                    return spool.canonical([{'Id': final_id}])
                if 'verify-finalized-development-image.py' in ' '.join(argv):
                    return spool.canonical({'installable': False, 'fixture': 'delegated verification mocked'})
                if 'push' in argv:
                    pushed = argv[-2]
                    pushed_config = loader_config if pushed == loader_id else config
                    pushed_manifest = spool.canonical({'schemaVersion': 2, 'config': {'digest': pushed}})
                    pushed_digest = 'sha256:' + spool.digest(pushed_manifest)
                    registry[pushed_digest] = (pushed_manifest, pushed_config)
                    Path(argv[argv.index('--digestfile') + 1]).write_text(pushed_digest)
                if argv[0] == 'skopeo':
                    digest = argv[-1].rsplit('@', 1)[-1]
                    found_manifest, found_config = registry[digest]
                    if '--format={{.Digest}}' in argv:
                        return digest.encode()
                    return found_config if '--config' in argv else found_manifest
                return b''
            driver.command = command
            result = driver.execute()
            self.assertTrue(result['published'])
            self.assertEqual(result['published_reference'], q['publish_repository'] + '@' + manifest_id)
            self.assertFalse(result['installable'])
            self.assertTrue(result['cosign_signing_required'])
            self.assertTrue((self.root / '08-published.json').is_file())
            prediction_timeout = next(timeout for argv, timeout in timed_calls
                                      if 'sign-target-prediction.sh' in ' '.join(argv))
            inspect_timeout = next(timeout for argv, timeout in timed_calls
                                   if argv[:4] == ['podman', '--remote=false', 'image', 'inspect'])
            self.assertGreater(prediction_timeout, q['signing_response_timeout_seconds'] + q['stage_timeout_seconds'])
            self.assertLessEqual(prediction_timeout, q['prediction_stage_timeout_seconds'])
            self.assertLessEqual(inspect_timeout, q['stage_timeout_seconds'])
            pushes = [c for c in calls if isinstance(c, list) and 'push' in c]
            self.assertEqual(len(pushes), 3)
            self.assertTrue(all(push[-1] != 'docker://' + q['new_target'] for push in pushes))

if __name__ == '__main__':
    unittest.main()
