import hashlib
import importlib.util
import io
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    'canonical', Path(__file__).parents[1] / 'apps/canonical_app_verifier.py'
)
c = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(c)


class CanonicalVerifierTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.pkg, self.runtime = self.root / 'tilo-app-package', self.root / 'tilo-runtime-bundle'
        self.pkg.write_bytes(b'pinned-package-tool')
        self.runtime.write_bytes(b'pinned-runtime-tool')
        self.policy = {'app_publishers': ['1' * 64], 'runtime_publisher': '2' * 64,
                       'canonical_binaries': {'tilo_pkg_sha256': c.file_sha(self.pkg), 'tilo_runtime_bundle_sha256': c.file_sha(self.runtime)}}
        self.verifier = c.CanonicalVerifier(self.pkg, self.runtime, self.policy)
        self.path = self.root / 'app.tapp'
        self.manifest = b'[package]\nid="dev.test.app"\nversion="1"\nruntime="tilo-runtime/1"\n'
        self.make_tapp()
        self.app = {'id': 'dev.test.app', 'version': '1', 'runtime': 'tilo-runtime/1'}
        self.package_id = 'tapp:' + '3' * 64

    def make_tapp(self, duplicate=False):
        with tarfile.open(self.path, 'w') as archive:
            for _ in range(2 if duplicate else 1):
                member = tarfile.TarInfo('manifest.toml')
                member.size = len(self.manifest)
                archive.addfile(member, io.BytesIO(self.manifest))

    def output(self, command, **kwargs):
        if command[1] == 'verify':
            self.assertEqual(command[2:4], ['--trusted-key', '1' * 64])
            return subprocess.CompletedProcess(command, 0, f'ok: {self.package_id} verifies and publisher is trusted (full)\n', '')
        self.assertEqual(command[1], 'inspect')
        return subprocess.CompletedProcess(command, 0, f'package-id: {self.package_id}\npublisher-identity: ed25519:{"1" * 64}\nsigned: yes\n\n--- manifest.toml ---\n', '')

    def test_canonical_verifier_accepts_only_trusted_signed_unchanged_packages(self):
        with self.subTest(phase='explicit trust and canonical identity report'):
            self.doCleanups()
            self.setUp()
            with patch.object(c.subprocess, 'run', side_effect=self.output) as run:
                self.assertEqual(self.verifier.verify_tapp(self.path, self.app), {'package_id': self.package_id, 'publisher': '1' * 64})
            self.assertEqual(run.call_count, 2)
        with self.subTest(phase='untrusted exit and unrecognized success are rejected'):
            self.doCleanups()
            self.setUp()
            for result in (subprocess.CompletedProcess([], 2, 'signature-ok-but-untrusted', ''),
                           subprocess.CompletedProcess([], 0, 'ok', '')):
                with self.subTest(result=result), patch.object(c.subprocess, 'run', return_value=result), self.assertRaises(ValueError):
                    self.verifier.verify_tapp(self.path, self.app)
        with self.subTest(phase='duplicate metadata and index mismatch rejected'):
            self.doCleanups()
            self.setUp()
            self.make_tapp(duplicate=True)
            with patch.object(c.subprocess, 'run') as run, self.assertRaisesRegex(ValueError, 'duplicate'):
                self.verifier.verify_tapp(self.path, self.app)
            run.assert_not_called()
            self.make_tapp()
            with patch.object(c.subprocess, 'run', side_effect=self.output), self.assertRaisesRegex(ValueError, 'collection index'):
                self.verifier.verify_tapp(self.path, dict(self.app, version='2'))
        with self.subTest(phase='runtime requires signature publisher and architecture'):
            self.doCleanups()
            self.setUp()
            directory = self.root / 'runtime'
            bundle = directory / 'tilo-runtime/1'
            bundle.mkdir(parents=True)
            (bundle / 'bundle.toml').write_text(f'version="1"\narch="x86_64"\nroot_hash="{"4" * 64}"\npublisher_hex="{"2" * 64}"\n')
            (directory / 'runtime-bundle-publisher.hex').write_text('2' * 64)
            with patch.object(c.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, '', '')) as run:
                self.verifier.verify_runtime(directory, 'x86_64')
            self.assertEqual(run.call_args.args[0][1:], ['verify', str(bundle), '--require-publisher', '2' * 64, '--require-signature', '--require-arch', 'x86_64'])
        with self.subTest(phase='binary change and empty trust set rejected'):
            self.doCleanups()
            self.setUp()
            self.pkg.write_bytes(b'replaced')
            with patch.object(c.subprocess, 'run') as run, self.assertRaisesRegex(ValueError, 'binary changed'):
                self.verifier.verify_tapp(self.path, self.app)
            run.assert_not_called()
            with self.assertRaisesRegex(ValueError, 'explicit app trust'):
                c.CanonicalVerifier(self.pkg, self.runtime, dict(self.policy, app_publishers=[]))


if __name__ == '__main__':
    unittest.main()
