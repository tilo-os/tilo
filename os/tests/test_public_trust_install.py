#!/usr/bin/env python3
"""Test production preservation and validated development installation."""
import importlib.util
import json
import os
from unittest import mock
from pathlib import Path
import shutil
import unittest
import test_development_trust as fixtures

spec = importlib.util.spec_from_file_location('install_public_trust', Path(__file__).resolve().parents[1] / 'dev/install-public-trust.py')
installer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(installer)
# The installer loads its own copy of development-trust.py. Keep it on the
# same locally built helper selected by the shared fixture suite.
installer.trust.CRYPTO = fixtures.trust.CRYPTO

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


class InstallTests(fixtures.PublicBundleTests):
    def setUp(self):
        super().setUp()
        self.official = Path(self.temporary.name) / 'official'
        self.official.mkdir()
        for name in installer.PRODUCTION_FILES:
            shutil.copyfile(self.output / name, self.official / name)
        self.image_root = Path(self.temporary.name) / 'image'
        self.destination = self.image_root / 'usr/share/tilo'
        self.digest = installer.trust.validate(self.output)['manifest_sha256']
        # Model immutable root-owned image files without requiring root to run
        # the unit tests; preserve real permissions and symlink/file types.
        original_lstat = Path.lstat
        def image_lstat(path, *args, **kwargs):
            info = list(original_lstat(path, *args, **kwargs))
            if path == self.image_root or self.image_root in path.parents:
                info[4] = 0
            return os.stat_result(info)
        patch = mock.patch.object(Path, 'lstat', image_lstat)
        patch.start()
        self.addCleanup(patch.stop)


    def run_install(self, source, mode, digest='', owner=''):
        installer.install(source, self.official, self.destination, mode, digest, owner, "127.0.0.1:5000/tilo-development/os:test", self.image_root)

    def test_production_install(self):
        with self.subTest(phase="production_preserves_exact_keys_and_no_marker"):
            self.run_install(self.official, 'production')
            self.assertEqual(set(p.name for p in self.destination.iterdir()), set(installer.PRODUCTION_FILES))
            for name in installer.PRODUCTION_FILES:
                self.assertEqual((self.destination / name).read_bytes(), (self.official / name).read_bytes())
        restart_fixture(self)
        with self.subTest(phase="production_rejects_development_bundle"):
            with self.assertRaises(ValueError):
                self.run_install(self.output, 'production')
            self.assertFalse(self.destination.exists())
        restart_fixture(self)
        with self.subTest(phase="production_rejects_substituted_keys"):
            source = Path(self.temporary.name) / 'substitute'
            shutil.copytree(self.official, source)
            (source / 'cosign.pub').write_text('substituted')
            with self.assertRaisesRegex(ValueError, 'differ'):
                self.run_install(source, 'production')
        restart_fixture(self)
        with self.subTest(phase="image_trust_is_installed_before_dracut"):
            containerfile = Path(__file__).resolve().parents[1] / 'Containerfile'
            source = containerfile.read_text()
            self.assertLess(
                source.index('python3 /run/tilo-trust-tools/install-public-trust.py'),
                source.index('bash /run/tilo-build-initramfs.sh "$kver"'),
            )

    def test_development_install(self):
        with self.subTest(phase="development_installs_pinned_keys_and_public_provenance"):
            self.run_install(self.output, 'development', self.digest, 'ab' * 32)
            self.assertEqual((self.destination / 'tpm2-pcr-signing.public.pem').read_bytes(), self.sources[1].read_bytes())
            provenance = json.loads((self.destination / 'development-trust/provenance.json').read_text())
            self.assertEqual(provenance['manifest_sha256'], self.digest)
            self.assertEqual(provenance['purpose'], 'tilo-development')
        restart_fixture(self)
        with self.subTest(phase="development_requires_pinned_digest_and_matching_owner"):
            for digest, owner in (('', 'ab' * 32), ('0' * 64, 'ab' * 32), (self.digest, 'cd' * 32)):
                with self.subTest(digest=digest, owner=owner), self.assertRaises(ValueError):
                    self.run_install(self.output, 'development', digest, owner)
            self.assertFalse(self.destination.exists())
        restart_fixture(self)
        with self.subTest(phase="development_origin_transport_and_repository_boundaries"):
            for value in ('https://dev.example/tilo-development/os:test',
                          'user:password@dev.example/tilo-development/os:test',
                          'registry.tilo.so/tilo-development/os:test',
                          'dev.example/tilo/tilo:test',
                          'dev.example:0/tilo-development/os:test',
                          'dev.example:05000/tilo-development/os:test',
                          'dev.example/tilo-development/os:test?query',
                          'localhost:5000/tilo-development/os:test'):
                with self.subTest(value=value), self.assertRaises(ValueError):
                    installer.origin.development_target(value)
            self.assertTrue(installer.origin.development_target('registry.vm.test:5443/tilo-development/os:test')['tls_required'])
            self.assertFalse(installer.origin.development_target('127.0.0.1:5000/tilo-development/os:test')['tls_required'])
            with self.assertRaises(ValueError):
                installer.origin.validate_target_imgref('registry.vm.test:5443/tilo-development/os:test')

    def test_runtime_registry_owner(self):
        with self.subTest(phase="runtime_binds_installed_registry_owner"):
            self.run_install(self.output, 'development', self.digest, 'ab' * 32)
            owner = self.destination / 'trust/tilo-main-registry-owner.pub'
            self.assertEqual(owner.read_bytes(), b'ab' * 32 + b'\n')
            self.assertIsNotNone(installer.origin.development_origin(self.image_root))
            for value in (b'cd' * 32, b'AB' * 32, b'0' * 64, b''):
                owner.write_bytes(value + b'\n')
                with self.subTest(value=value), self.assertRaisesRegex(ValueError, 'registry owner differs'):
                    installer.origin.development_origin(self.image_root)
        restart_fixture(self)
        with self.subTest(phase="runtime_rejects_redirected_or_writable_registry_owner"):
            self.run_install(self.output, 'development', self.digest, 'ab' * 32)
            owner = self.destination / 'trust/tilo-main-registry-owner.pub'
            owner.chmod(0o666)
            with self.assertRaisesRegex(ValueError, 'not group/world writable'):
                installer.origin.development_origin(self.image_root)
            owner.unlink()
            owner.symlink_to(self.destination / 'development-trust/registry-owner.pub')
            with self.assertRaisesRegex(ValueError, 'symlink'):
                installer.origin.development_origin(self.image_root)
        restart_fixture(self)
        with self.subTest(phase="runtime_rejects_non_root_registry_owner"):
            self.run_install(self.output, 'development', self.digest, 'ab' * 32)
            owner = self.destination / 'trust/tilo-main-registry-owner.pub'
            original_lstat = Path.lstat
            def substituted_owner(path, *args, **kwargs):
                info = list(original_lstat(path, *args, **kwargs))
                if path == owner:
                    info[4] = 1000
                return os.stat_result(info)
            with mock.patch.object(Path, 'lstat', substituted_owner):
                with self.assertRaisesRegex(ValueError, 'root-owned'):
                    installer.origin.development_origin(self.image_root)
        restart_fixture(self)
        with self.subTest(phase="runtime_resolves_only_bound_development_origin"):
            self.run_install(self.output, 'development', self.digest, 'ab' * 32)
            bound = "127.0.0.1:5000/tilo-development/os:test"
            self.assertEqual(installer.origin.install_update_origin(self.image_root), bound)
            self.assertEqual(installer.origin.install_update_origin(self.image_root, bound), bound)
            for ref in ("registry.tilo.so/tilo/tilo:latest", "127.0.0.1:5000/tilo-development/os:other"):
                with self.assertRaises(ValueError):
                    installer.origin.install_update_origin(self.image_root, ref)
        restart_fixture(self)
        with self.subTest(phase="runtime_rejects_missing_or_writable_origin"):
            self.run_install(self.output, 'development', self.digest, 'ab' * 32)
            origin = self.destination / 'development-trust/origin.json'
            origin.chmod(0o666)
            with self.assertRaisesRegex(ValueError, 'not group/world writable'):
                installer.origin.install_update_origin(self.image_root)
            origin.unlink()
            with self.assertRaises(OSError):
                installer.origin.install_update_origin(self.image_root)

    def test_runtime_policy_and_keys(self):
        with self.subTest(phase="runtime_rejects_weakened_policy"):
            self.run_install(self.output, 'development', self.digest, 'ab' * 32)
            policy = self.image_root / 'etc/containers/policy.json'
            document = json.loads(policy.read_text())
            document['default'] = [{'type': 'insecureAcceptAnything'}]
            policy.write_text(json.dumps(document))
            with self.assertRaisesRegex(ValueError, 'policy must require'):
                installer.origin.install_update_origin(self.image_root)
        restart_fixture(self)
        with self.subTest(phase="remote_registry_requires_https_and_exact_signer"):
            installer.install(self.output, self.official, self.destination, 'development', self.digest,
                              'ab' * 32, 'registry.vm.test:5443/tilo-development/os:test', self.image_root)
            self.assertEqual(installer.origin.install_update_origin(self.image_root),
                             'registry.vm.test:5443/tilo-development/os:test')
            transport = self.image_root / 'etc/containers/registries.conf.d/99-tilo-development.conf'
            self.assertIn('insecure = false', transport.read_text())
            original = transport.read_text()
            transport.write_text(original.replace('false', 'true'))
            with self.assertRaisesRegex(ValueError, 'transport'):
                installer.origin.install_update_origin(self.image_root)
            transport.write_text(original)
            policy = self.image_root / 'etc/containers/policy.json'
            document = json.loads(policy.read_text())
            document['transports']['docker']['registry.vm.test:5443/tilo-development/os'][0]['keyPath'] = '/tmp/untrusted.pub'
            policy.write_text(json.dumps(document))
            with self.assertRaisesRegex(ValueError, 'policy'):
                installer.origin.install_update_origin(self.image_root)
        restart_fixture(self)
        with self.subTest(phase="runtime_rejects_key_and_manifest_substitution"):
            self.run_install(self.output, 'development', self.digest, 'ab' * 32)
            key = self.destination / 'cosign.pub'
            original = key.read_bytes()
            key.write_bytes(original + b'\n')
            with self.assertRaisesRegex(ValueError, 'installed development key differs'):
                installer.origin.install_update_origin(self.image_root)
            key.write_bytes(original)
            manifest = self.destination / 'development-trust/manifest.json'
            manifest.write_bytes(manifest.read_bytes() + b'\n')
            with self.assertRaisesRegex(ValueError, 'manifest binding differs'):
                installer.origin.install_update_origin(self.image_root)

    def test_retained_registry(self):
        with self.subTest(phase="retained_registry_has_exact_namespace_and_native_tags"):
            repository = 'registry.tilo.so/tilo/tilo-development'
            for architecture in ('x86_64', 'aarch64'):
                selected = installer.origin.development_target(repository + ':development-' + architecture, architecture)
                self.assertEqual(selected['repository'], repository)
                self.assertTrue(selected['tls_required'])
            installer.origin.development_target(repository + '@sha256:' + 'a' * 64, 'aarch64')
            for value in ('registry.tilo.so/tilo/tilo:development-aarch64',
                          'registry.tilo.so/tilo/tilo-development/child:development-aarch64',
                          'registry.tilo.so:443/tilo/tilo-development:development-aarch64',
                          'registry.tilo.so./tilo/tilo-development:development-aarch64',
                          'code.tilo.so/tilo/tilo-development:development-aarch64',
                          'alias.example/tilo/tilo-development:development-aarch64',
                          repository + ':latest', repository + ':development-x86_64'):
                with self.subTest(value=value), self.assertRaises(ValueError):
                    installer.origin.development_target(value, 'aarch64')
            with self.assertRaises(ValueError):
                installer.origin.validate_target_imgref(repository + ':development-aarch64')
        restart_fixture(self)
        with self.subTest(phase="retained_origin_installs_exact_policy_and_rejects_override"):
            repository = 'registry.tilo.so/tilo/tilo-development'
            target = repository + ':development-aarch64'
            installer.install(self.output, self.official, self.destination, 'development', self.digest,
                              'ab' * 32, target, self.image_root)
            self.assertEqual(installer.origin.install_update_origin(self.image_root), target)
            policy = json.loads((self.image_root / 'etc/containers/policy.json').read_text())
            self.assertEqual(policy, installer.origin.development_policy(repository))
            self.assertIn('insecure = false', (self.image_root / 'etc/containers/registries.conf.d/99-tilo-development.conf').read_text())
            for override in (repository + ':development-x86_64', 'registry.tilo.so/tilo/tilo:latest'):
                with self.subTest(override=override), self.assertRaises(ValueError):
                    installer.origin.install_update_origin(self.image_root, override)
            (self.destination / 'cosign.pub').write_bytes(b'substituted')
            with self.assertRaisesRegex(ValueError, 'installed development key differs'):
                installer.origin.install_update_origin(self.image_root)

class InstallFailureTests(unittest.TestCase):
    """Exercise filesystem and manifest handling without a native crypto build."""

    def setUp(self):
        import hashlib
        import tempfile
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.source = self.root / 'bundle'
        self.source.mkdir()
        for name in installer.trust.FILES:
            label = 'CERTIFICATE' if name.endswith('.crt') else 'PUBLIC KEY'
            data = (f'-----BEGIN {label}-----\nYWJj\n-----END {label}-----\n').encode()
            (self.source / name).write_bytes(b'ab' * 32 if name == 'registry-owner.pub' else data)
        manifest = {'schema': 1, 'purpose': 'tilo-development', 'sha256': {
            name: hashlib.sha256((self.source / name).read_bytes()).hexdigest()
            for name in installer.trust.FILES}}
        raw = json.dumps(manifest).encode()
        (self.source / 'manifest.json').write_bytes(raw)
        self.digest = hashlib.sha256(raw).hexdigest()
        self.official = self.root / 'official'
        self.official.mkdir()
        for name in installer.PRODUCTION_FILES:
            (self.official / name).write_bytes((self.source / name).read_bytes())
        self.image = self.root / 'image'
        self.destination = self.image / 'usr/share/tilo'
        self.crypto = mock.patch.object(installer.trust, 'rust_crypto', side_effect=lambda op, path:
            (b'2048 ' + b'1' * 64 if op == 'rsa-public-key-info' else b'2' * 64))
        self.crypto.start()
        self.addCleanup(self.crypto.stop)

    def install(self, mode='development'):
        installer.install(self.source if mode == 'development' else self.official,
                          self.official, self.destination, mode,
                          self.digest if mode == 'development' else '',
                          'ab' * 32, '127.0.0.1:5000/tilo-development/os:test', self.image)

    def test_installs_only_the_validated_bundle_snapshot(self):
        before = {name: (self.source / name).read_bytes()
                  for name in (*installer.trust.FILES, 'manifest.json')}
        validate = installer.trust.validate
        def validate_then_change(*args, **kwargs):
            result = validate(*args, **kwargs)
            for name in before:
                (self.source / name).write_bytes(b'substituted after validation')
            return result
        with mock.patch.object(installer.trust, 'validate', side_effect=validate_then_change):
            self.install()
        for name, data in before.items():
            self.assertEqual((self.destination / 'development-trust' / name).read_bytes(), data)
        for name in installer.PRODUCTION_FILES:
            self.assertEqual((self.destination / name).read_bytes(), before[name])

    def test_late_conflicts_leave_all_existing_trust_untouched(self):
        for relative, directory in [('usr/share/tilo/development-trust', True),
                                    ('etc/containers/policy.json', True),
                                    ('etc/containers/registries.d', False)]:
            with self.subTest(relative=relative):
                shutil.rmtree(self.image, ignore_errors=True)
                self.destination.mkdir(parents=True)
                key = self.destination / 'cosign.pub'
                key.write_bytes(b'old trusted key')
                conflict = self.image / relative
                conflict.parent.mkdir(parents=True, exist_ok=True)
                conflict.mkdir() if directory else conflict.write_bytes(b'not a directory')
                with self.assertRaises((ValueError, OSError)):
                    self.install()
                self.assertEqual(key.read_bytes(), b'old trusted key')
                self.assertFalse((self.destination / 'tpm2-pcr-signing.public.pem').exists())
                self.assertFalse((self.destination / 'trust').exists())

    def test_redirected_outputs_never_modify_external_files(self):
        for kind in ('key-link', 'key-hardlink', 'parent-link', 'policy-link'):
            with self.subTest(kind=kind):
                shutil.rmtree(self.image, ignore_errors=True)
                self.destination.mkdir(parents=True)
                external = self.root / 'external'
                if external.exists():
                    shutil.rmtree(external)
                external.mkdir()
                victim = external / 'victim'
                victim.write_bytes(b'preserve outside image')
                if kind in ('key-link', 'key-hardlink'):
                    path = self.destination / 'cosign.pub'
                    path.symlink_to(victim) if kind == 'key-link' else os.link(victim, path)
                elif kind == 'parent-link':
                    shutil.rmtree(self.destination)
                    self.destination.symlink_to(external, target_is_directory=True)
                else:
                    path = self.image / 'etc/containers/policy.json'
                    path.parent.mkdir(parents=True)
                    path.symlink_to(victim)
                with self.assertRaises(ValueError):
                    self.install()
                self.assertEqual(victim.read_bytes(), b'preserve outside image')
                self.assertFalse((self.destination / 'tpm2-pcr-signing.public.pem').exists())

    def test_production_rejects_stale_development_metadata(self):
        marker = self.destination / 'development-trust'
        marker.mkdir(parents=True)
        key = self.destination / 'cosign.pub'
        key.write_bytes(b'old development key')
        with self.assertRaisesRegex(ValueError, 'development trust'):
            self.install('production')
        self.assertEqual(key.read_bytes(), b'old development key')
        self.assertTrue(marker.is_dir())

    def test_clean_production_and_development_installs_remain_supported(self):
        self.install('production')
        self.assertEqual((self.destination / 'cosign.pub').read_bytes(),
                         (self.official / 'cosign.pub').read_bytes())
        self.install()
        self.assertEqual(json.loads((self.destination / 'development-trust/origin.json').read_text())
                         ['manifest_sha256'], self.digest)


if __name__ == '__main__':
    unittest.main()
