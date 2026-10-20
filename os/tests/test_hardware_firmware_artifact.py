"""Firmware import boundaries without registry access or image execution."""
import hashlib
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch
from restart_fixture import restart_fixture

DEV = Path(__file__).resolve().parents[1] / 'dev'
sys.path.insert(0, str(DEV))
spec = importlib.util.spec_from_file_location('firmware_artifact', DEV / 'hardware-firmware-artifact.py')
artifact = importlib.util.module_from_spec(spec)
spec.loader.exec_module(artifact)


class ImportTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.base = Path(self.temporary.name)
        self.repo = self.base / 'repo'
        definitions = self.repo / 'os/hardware/profiles'
        definitions.mkdir(parents=True)
        (self.repo / 'os/hardware/firmware').mkdir()
        definition = {'schema_version': 1, 'id': 'board', 'architecture': 'aarch64',
                      'devicetree': None, 'firmware': {'radio.bin': 'vendor/radio.bin'}}
        (definitions / 'board.json').write_text(json.dumps(definition))
        self.root = self.base / 'mounted'
        self.firmware = self.root / 'firmware'
        self.firmware.mkdir(parents=True)
        (self.firmware / 'radio.bin').write_bytes(b'firmware')
        manifest = {'schema_version': 1, 'profile': 'board',
                    'files': {'radio.bin': hashlib.sha256(b'firmware').hexdigest()}}
        raw = json.dumps(manifest).encode()
        (self.firmware / 'manifest.json').write_bytes(raw)
        self.args = SimpleNamespace(repo=self.repo, profile='board', architecture='aarch64',
            reference='code.tilo.so/tilo/firmware@sha256:' + '1' * 64,
            manifest_sha256=hashlib.sha256(raw).hexdigest())
        self.info = {'Id': 'sha256:' + '2' * 64, 'Os': 'linux', 'Architecture': 'arm64',
                     'Digest': 'sha256:' + '1' * 64, 'RepoDigests': [self.args.reference]}
        self.calls = []
        self.fail_unmount = False
        for owner, name, value in ((artifact.sys, 'platform', 'linux'),
                                  (artifact.os, 'getuid', lambda: 0),
                                  (artifact.os, 'geteuid', lambda: 0),
                                  (artifact.platform, 'machine', lambda: 'aarch64')):
            patcher = patch.object(owner, name, value)
            patcher.start()
            self.addCleanup(patcher.stop)
        patcher = patch.dict(artifact.os.environ, {}, clear=True)
        patcher.start()
        self.addCleanup(patcher.stop)

    def command(self, *args, storage):
        self.assertTrue(storage.is_absolute())
        self.assertTrue(storage.is_dir())
        self.calls.append((args, storage))
        if args[0] == 'pull': return ''
        if args[:2] == ('image', 'inspect'): return json.dumps([self.info])
        if args[:2] == ('image', 'mount'): return str(self.root)
        if args[:2] == ('image', 'unmount'):
            if self.fail_unmount: raise ValueError('unmount failed')
            return ''
        raise AssertionError(args)

    def execute(self):
        with patch.object(artifact, 'command', self.command):
            return artifact.import_artifact(self.args)

    @property
    def destination(self):
        return self.repo / 'os/hardware/firmware/board'

    def test_verified_import(self):
        with self.subTest(phase="verified_payload_staged_and_identical_repeat_allowed"):
            receipt = self.execute()
            self.assertEqual((self.destination / 'radio.bin').read_bytes(), b'firmware')
            self.assertEqual(receipt['manifest_sha256'], self.args.manifest_sha256)
            self.assertEqual(self.execute(), receipt)
            self.assertEqual(self.calls[-1][0][:2], ('image', 'unmount'))
            self.assertTrue(all(call[0][0] in ('pull', 'image') for call in self.calls))
            self.assertEqual(len({storage for _, storage in self.calls[:4]}), 1)
            self.assertEqual(len({storage for _, storage in self.calls[4:]}), 1)
            self.assertNotEqual(self.calls[0][1], self.calls[4][1])
            self.assertTrue(all(not storage.exists() for _, storage in self.calls))
        restart_fixture(self)
        with self.subTest(phase="existing_different_files_preserved"):
            self.execute()
            (self.destination / 'radio.bin').write_bytes(b'original')
            with self.assertRaisesRegex(ValueError, 'existing firmware'):
                self.execute()
            self.assertEqual((self.destination / 'radio.bin').read_bytes(), b'original')
        restart_fixture(self)
        with self.subTest(phase="nested_podman_uses_private_vfs_store"):
            store = self.base / 'private-store'
            with patch.object(artifact.subprocess, 'run') as run:
                run.return_value.returncode = 0
                run.return_value.stdout = b''
                artifact.command('pull', self.args.reference, storage=store)
            argv = run.call_args.args[0]
            self.assertEqual(argv[:5], ['podman', '--remote=false', '--storage-driver=vfs',
                                        f'--root={store / "graph"}', f'--runroot={store / "run"}'])
            self.assertEqual(argv[5:], ['pull', self.args.reference])
            self.assertNotIn('REGISTRY_TOKEN', run.call_args.kwargs['env'])

    def test_corrupted_payload_never_staged(self):
        with self.subTest(phase="independent_manifest_mismatch_unmounts_without_staging"):
            self.args.manifest_sha256 = '0' * 64
            with self.assertRaisesRegex(ValueError, 'independent digest'):
                self.execute()
            self.assertFalse(self.destination.exists())
            self.assertEqual(self.calls[-1][0][:2], ('image', 'unmount'))
        restart_fixture(self)
        with self.subTest(phase="blob_corruption_unmounts_without_staging"):
            (self.firmware / 'radio.bin').write_bytes(b'corrupt')
            with self.assertRaisesRegex(ValueError, 'hash differs'):
                self.execute()
            self.assertFalse(self.destination.exists())
            self.assertEqual(self.calls[-1][0][:2], ('image', 'unmount'))
        restart_fixture(self)
        with self.subTest(phase="unknown_file_rejected"):
            (self.firmware / 'extra').write_text('extra')
            with self.assertRaisesRegex(ValueError, 'exactly'):
                self.execute()
            self.assertFalse(self.destination.exists())
        restart_fixture(self)
        with self.subTest(phase="symlink_rejected"):
            path = self.firmware / 'radio.bin'
            path.unlink()
            path.symlink_to(self.firmware / 'manifest.json')
            with self.assertRaisesRegex(ValueError, 'symlink'):
                self.execute()
            self.assertFalse(self.destination.exists())
        restart_fixture(self)
        with self.subTest(phase="failed_unmount_never_stages"):
            self.fail_unmount = True
            with self.assertRaisesRegex(ValueError, 'unmount failed'):
                self.execute()
            self.assertFalse(self.destination.exists())

    def test_reference_rejected_before_tools(self):
        with self.subTest(phase="architecture_or_digest_mismatch_never_mounts"):
            for key, value in [('Architecture', 'amd64'), ('Digest', 'sha256:' + '3' * 64), ('RepoDigests', [])]:
                with self.subTest(key=key), patch.dict(self.info, {key: value}):
                    self.calls.clear()
                    with self.assertRaisesRegex(ValueError, 'identity or architecture'):
                        self.execute()
                    self.assertFalse(any(call[0][:2] == ('image', 'mount') for call in self.calls))
        restart_fixture(self)
        with self.subTest(phase="mutable_reference_rejected_before_tools"):
            self.args.reference = 'code.tilo.so/tilo/firmware:latest'
            with self.assertRaisesRegex(ValueError, 'immutable'):
                self.execute()
            self.assertEqual(self.calls, [])


if __name__ == '__main__':
    unittest.main()
