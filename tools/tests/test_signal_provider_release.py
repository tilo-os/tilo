"""Protect explicit release admission and refusal before signing or upload."""
import importlib.util
import hashlib
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

    def test_app_kind_is_refused_before_release_provenance_is_admitted(self):
        with patch.object(release.tomllib, 'loads', return_value={
            'package': {'runtime': release.RUNTIME, 'kind': 'app'},
        }):
            with self.assertRaisesRegex(ValueError, 'headless service kind'):
                release.provenance(release.release.head())

    def test_jit_profile_is_refused_without_using_signing_authority(self):
        with patch.object(release.tomllib, 'loads', return_value={
            'package': {'runtime': release.RUNTIME, 'kind': 'service'},
            'sandbox': {'seccomp': 'tilo-jit'},
        }):
            with self.assertRaisesRegex(ValueError, 'retain its strict sandbox'):
                release.provenance(release.release.head())

    def compiled(self, root):
        (root / 'bin').mkdir()
        (root / 'bin/tool').write_bytes(b'compiled release tool')
        (root / 'signal-provider').mkdir()
        (root / 'signal-provider/provider').write_bytes(b'compiled pinned provider')
        proof = release.provenance(release.release.head())
        java = root / 'signal-provider/java'
        (java / 'lib/server').mkdir(parents=True)
        (java / 'lib/server/libjvm.so').write_bytes(b'synthetic Zero VM')
        (java / 'release').write_text('JVM_VARIANT="Zero"\n')
        modified = root / 'signal-provider/java-zero-source' / release.zero.RESERVER
        modified.parent.mkdir(parents=True)
        modified.write_text(release.zero.ZERO_RESERVATION)
        (root / 'signal-provider/java-zero.json').write_text(json.dumps({
            'source': proof['pin']['java_zero_source'], 'variant': 'zero',
            'java_version': proof['pin']['java_version'],
            'vm_sha256': hashlib.sha256(b'synthetic Zero VM').hexdigest(),
            'data_only_cache_source_sha256': hashlib.sha256(modified.read_bytes()).hexdigest(),
        }))
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

    def test_sealing_uses_the_existing_payload_bound_native_bundle_builder(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            proof = self.compiled(root)
            version = release.RUNTIME.removeprefix('tilo-runtime/') + '+native.' + 'a' * 32
            environment = {'RUNTIME_BUNDLE_IDENTITY_FILE': '/fixture/existing-runtime',
                           'TAPP_PUBLISHER_IDENTITY_FILE': '/fixture/existing-app'}

            def built(args, **kwargs):
                if args[1] != 'build':
                    self.assertEqual(args[1:3], ['verify', str(root / 'bundle')])
                    self.assertTrue((root / 'bundle/bundle.toml').exists())
                    raise RuntimeError('fixture stops before signing packages')
                self.assertNotIn('--version', args)
                self.assertEqual(args[args.index('--release') + 1], '2026.10.1-signal')
                destination = Path(args[args.index('--out-root') + 1]) / version
                destination.mkdir(parents=True)
                (destination / 'bundle.toml').write_text('version = "' + version + '"\n')
                Path(args[args.index('--version-out') + 1]).write_text(version + '\n')

            with patch.dict(os.environ, environment, clear=True), \
                    patch.object(release.release, 'head', return_value=proof['source']), \
                    patch.object(release.subprocess, 'check_output',
                                 return_value=proof['runtime_publisher']), \
                    patch.object(release.subprocess, 'run', side_effect=built):
                with self.assertRaisesRegex(RuntimeError, 'stops before signing'):
                    release.seal(root, proof['source'], '1')

    def test_canonical_entrypoint_defaults_keep_the_exact_executable_policy(self):
        source = release.release.head()
        trusted_key = 'f' * 64
        version = '0.1.1+g' + source
        canonical = ('bin = "bin/tilo-signal-provider"\n'
                     'type = "cli"\n'
                     'splash-segments = []\n'
                     'splash-budget = 100\n')
        runtime_entrypoint = canonical.replace('bin/tilo-signal-provider', release.RUNTIME_ENTRYPOINT)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            proof = release.provenance(source)
            runtime_build = release.RUNTIME + '+native.' + 'a' * 32
            proof.update(package_version=version, app_publisher=trusted_key,
                         runtime_build=runtime_build)
            proof['packages'] = {}
            for name in ('runtime-package', 'provider-package'):
                package = root / name
                package.mkdir()
                (package / 'fixture.tapp').write_bytes(b'fixture package bytes')
                proof['packages'][name] = release.inventory(package)
            (root / 'release.json').write_text(json.dumps(proof))

            def inspected(args, **kwargs):
                runtime = Path(args[-1]).parent.name == 'runtime-package'
                declaration = ('id = "so.tilo.runtime.signal"\nkind = "runtime"\nprovides' if runtime
                               else 'id = "so.tilo.signal-provider"\nkind = "' + declared_kind + '"\nruntime')
                return ('--- manifest.toml ---\n[package]\n' + declaration
                        + ' = "' + declared_runtime + '"\nversion = "' + version + '"\n'
                        + '[sandbox.net]\nmode = "off"\n'
                        + '[entrypoint]\n' + (runtime_entrypoint if runtime else entrypoint)
                        + '--- payload (0 files) ---\n')

            declared_runtime = runtime_build
            declared_kind = 'service'
            for entrypoint in (canonical,
                               canonical.replace('bin/tilo-signal-provider', 'bin/other'),
                               canonical.replace('type = "cli"', 'type = "both"'),
                               canonical + 'cli-bin = "bin/other"\n',
                               canonical + 'gui-bin = "bin/other"\n',
                               canonical.replace('splash-segments = []',
                                                 'splash-segments = ["bin/other"]'),
                               canonical.replace('splash-budget = 100', 'splash-budget = 101')):
                with self.subTest(entrypoint=entrypoint), \
                        patch.object(release.release, 'head', return_value=source), \
                        patch.object(release.subprocess, 'run') as verify_signature, \
                        patch.object(release.subprocess, 'check_output', side_effect=inspected):
                    if entrypoint == canonical:
                        release.verify(root, source, '1', trusted_key)
                    else:
                        with self.assertRaisesRegex(ValueError, 'executable declaration differs'):
                            release.verify(root, source, '1', trusted_key)
                    self.assertEqual(verify_signature.call_count, 2)
                    for call in verify_signature.call_args_list:
                        self.assertEqual(call.args[0][1:4], ['verify', '--trusted-key', trusted_key])

            entrypoint = canonical
            for invalid in ('', runtime_entrypoint.replace(release.RUNTIME_ENTRYPOINT, 'lib/other'),
                            runtime_entrypoint + 'cli-bin = "lib/other"\n'):
                with self.subTest(runtime_entrypoint=invalid), \
                        patch.object(release.release, 'head', return_value=source), \
                        patch.object(release.subprocess, 'run'), \
                        patch.object(release.subprocess, 'check_output', side_effect=inspected):
                    saved = runtime_entrypoint
                    runtime_entrypoint = invalid
                    try:
                        with self.assertRaisesRegex(ValueError, 'executable declaration differs'):
                            release.verify(root, source, '1', trusted_key)
                    finally:
                        runtime_entrypoint = saved

            for declared_runtime in (release.RUNTIME,
                                     release.RUNTIME + '+native.' + 'b' * 32):
                with self.subTest(declared_runtime=declared_runtime), \
                        patch.object(release.release, 'head', return_value=source), \
                        patch.object(release.subprocess, 'run'), \
                        patch.object(release.subprocess, 'check_output', side_effect=inspected):
                    with self.assertRaisesRegex(ValueError, 'runtime/network declaration differs'):
                        release.verify(root, source, '1', trusted_key)

            declared_runtime = runtime_build
            declared_kind = 'app'
            with patch.object(release.release, 'head', return_value=source), \
                    patch.object(release.subprocess, 'run'), \
                    patch.object(release.subprocess, 'check_output', side_effect=inspected):
                with self.assertRaisesRegex(ValueError, 'package declaration differs'):
                    release.verify(root, source, '1', trusted_key)

            proof['runtime_build'] = release.RUNTIME
            (root / 'release.json').write_text(json.dumps(proof))
            with patch.object(release.release, 'head', return_value=source), \
                    patch.object(release.subprocess, 'run') as execute:
                with self.assertRaisesRegex(ValueError, 'exact native build'):
                    release.verify(root, source, '1', trusted_key)
                execute.assert_not_called()

    def test_inventory_refuses_links(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'escape').symlink_to('/etc/passwd')
            with self.assertRaisesRegex(ValueError, 'contains a link'):
                release.inventory(root)


if __name__ == '__main__':
    unittest.main()
