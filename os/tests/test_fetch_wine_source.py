#!/usr/bin/env python3
"""Exercise source acquisition publication, pinned bytes and failure evidence."""
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

DEV = Path(__file__).resolve().parents[1] / 'dev'
sys.path.insert(0, str(DEV))
SPEC = importlib.util.spec_from_file_location('fetch_wine_source', DEV / 'fetch-wine-source.py')
F = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(F)


class FetchTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.key = self.root / 'installed-key'
        self.key.write_bytes(b'key fixture')
        self.rpm = b'RPM fixture'
        self.pin = {'filename': 'wine-11.0-3.fc44.src.rpm', 'version': '11.0', 'release': '3.fc44',
                    'sha256': hashlib.sha256(self.rpm).hexdigest(), 'size': len(self.rpm),
                    'fedora_key_sha256': hashlib.sha256(self.key.read_bytes()).hexdigest()}
        self.pin_file = self.root / 'pin.json'
        self.pin_file.write_text(json.dumps(self.pin))
        self.out = self.root / 'output'
        self.host = patch.object(F, 'require_host')
        self.host.start()
        self.addCleanup(self.host.stop)

    def download(self, argv, **kwargs):
        self.assertEqual(argv[:3], ['dnf5', 'download', '--source'])
        self.assertEqual(argv[-1], 'wine-11.0-3.fc44')
        stage = Path(argv[3].removeprefix('--destdir='))
        (stage / self.pin['filename']).write_bytes(self.rpm)
        kwargs['stdout'].write(b'DNF downloaded source\n')

    def fetch(self, rpmkeys='    Header V4 RSA/SHA256 Signature: OK'):
        with patch.object(F.subprocess, 'run', side_effect=self.download), \
                patch.object(F.contract, 'run', return_value=rpmkeys):
            return F.acquire(self.out, self.pin_file, self.key)

    def test_success_verifies_bytes_and_publishes_logs(self):
        self.fetch()
        self.assertEqual((self.out / self.pin['filename']).read_bytes(), self.rpm)
        self.assertIn('Signature', (self.out / 'signature.log').read_text())
        self.assertIn('DNF downloaded', (self.out / 'download.log').read_text())
        self.assertTrue(json.loads((self.out / 'acquisition.json').read_text())['signature_verified'])
        self.assertFalse(list(self.root.glob('output.fetch-*')))

    def test_wrong_key_never_downloads(self):
        self.key.write_bytes(b'wrong')
        with patch.object(F.subprocess, 'run') as run, self.assertRaises(ValueError):
            F.acquire(self.out, self.pin_file, self.key)
        run.assert_not_called()
        self.assertFalse(self.out.exists())

    def test_rpm6_lowercase_signature_is_authenticated(self):
        result = '    Header OpenPGP V4 RSA/SHA256 signature, key fingerprint: abc: OK'
        self.fetch(result)
        self.assertEqual((self.out / 'signature.log').read_text(), result + '\n')

    def test_unsigned_and_nonpositive_results_never_publish(self):
        for result in (
                '    Header SHA256 digest: OK\n    Payload SHA256 digest: OK',
                '    Header OpenPGP V4 RSA/SHA256 signature: NOT OK',
                '    Header OpenPGP V4 RSA/SHA256 signature: NOKEY',
                '    Header OpenPGP V4 RSA/SHA256 signature: OK\n    Payload SHA256 digest: NOT OK',
                '/path/Signature: OK'):
            with self.subTest(result=result), self.assertRaises(RuntimeError):
                self.fetch(result)
            self.assertFalse(self.out.exists())
        self.assertTrue(all(not p.name.startswith('.') for p in self.root.glob('output.fetch-*')))

    def test_tampered_rpm_rejected_before_rpmkeys(self):
        self.rpm = b'tampered RPM'
        with patch.object(F.subprocess, 'run', side_effect=self.download), \
                patch.object(F.contract, 'run') as rpmkeys, self.assertRaises(RuntimeError):
            F.acquire(self.out, self.pin_file, self.key)
        rpmkeys.assert_not_called()
        self.assertFalse(self.out.exists())
        self.assertTrue(next(self.root.glob('output.fetch-*')).joinpath('failure.log').is_file())

    def test_bad_signature_never_publishes(self):
        with self.assertRaises(RuntimeError):
            self.fetch('Header V4 RSA/SHA256 Signature: NOKEY')
        self.assertFalse(self.out.exists())

    def test_unexpected_second_rpm_rejected(self):
        def extra(argv, **kwargs):
            self.download(argv, **kwargs)
            stage = Path(argv[3].removeprefix('--destdir='))
            (stage / 'unexpected.src.rpm').write_bytes(b'extra')
        with patch.object(F.subprocess, 'run', side_effect=extra), \
                patch.object(F.contract, 'run') as rpmkeys, self.assertRaises(RuntimeError):
            F.acquire(self.out, self.pin_file, self.key)
        rpmkeys.assert_not_called()
        self.assertFalse(self.out.exists())

    def test_pin_filename_cannot_escape_output(self):
        self.pin['filename'] = '../wine-11.0-3.fc44.src.rpm'
        self.pin_file.write_text(json.dumps(self.pin))
        with self.assertRaises(ValueError):
            self.fetch()
        self.assertFalse(list(self.root.glob('output.fetch-*')))

    def test_download_failure_retains_log(self):
        def fail(argv, **kwargs):
            kwargs['stdout'].write(b'repository unavailable\n')
            raise subprocess.CalledProcessError(1, argv)
        with patch.object(F.subprocess, 'run', side_effect=fail), self.assertRaises(RuntimeError):
            F.acquire(self.out, self.pin_file, self.key)
        stage = next(self.root.glob('output.fetch-*'))
        self.assertIn('repository unavailable', (stage / 'download.log').read_text())
        self.assertFalse(self.out.exists())

    def test_existing_output_unchanged(self):
        self.out.mkdir()
        with self.assertRaises(FileExistsError):
            self.fetch()
        self.assertEqual(list(self.out.iterdir()), [])

    def test_publication_race_cannot_replace_empty_directory(self):
        stage = self.root / 'stage'
        stage.mkdir()
        (stage / 'marker').write_text('retained')
        self.out.mkdir()
        with self.assertRaises(FileExistsError):
            F.publish(stage, self.out)
        self.assertEqual(list(self.out.iterdir()), [])
        self.assertTrue((stage / 'marker').exists())


if __name__ == '__main__':
    unittest.main()
