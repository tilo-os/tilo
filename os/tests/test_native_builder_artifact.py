"""Native builder artifact boundary tests; no compiler, registry or image runs."""
from contextlib import contextmanager
import importlib.util
import json
import os
from pathlib import Path
import struct
import tempfile
import unittest
from unittest.mock import patch

DEV = Path(__file__).resolve().parents[1] / 'dev'
spec = importlib.util.spec_from_file_location('artifact', DEV / 'native_builder_artifact.py')
artifact = importlib.util.module_from_spec(spec)
spec.loader.exec_module(artifact)


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


@unittest.skipUnless(os.getuid() == 0, 'mounted image ownership checks require root')
class ArtifactTests(unittest.TestCase):
    ENGINE = 'usr/lib/tilo-runtime/2026.10.0-chromium/lib/chromium/tilo-chromium-host'

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.inputs = {'architecture': 'aarch64', 'runtime_version': '2026.07',
                       'engine_runtime_version': '2026.10.0-chromium', 'revision': 'a' * 40}
        header = bytearray(64)
        header[:7] = b'\x7fELF\x02\x01\x01'
        struct.pack_into('<HH', header, 16, 3, 183)
        for name in artifact.REQUIRED:
            self.write('out/' + name, bytes(header), 0o755)
        self.write(self.ENGINE, bytes(header), 0o755)
        for name in ('runtime-bundle-publisher.hex', 'app-publisher.hex'):
            self.write('out/' + name, b'1' * 64 + b'\n')
        self.write('usr/lib/tilo-runtime/2026.07+native.' + '0' * 32 + '/bundle.json', b'{}')
        self.write('usr/share/tilo-apps/registry.toml', b'fixture')

    def write(self, name, data, mode=0o644):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        path.chmod(mode)

    def test_payload_contents(self):
        with self.subTest(phase="complete_native_payload_and_safe_symlink"):
            (self.root / 'out/shell-alias').symlink_to('shell')
            result = artifact.inventory(self.root, self.inputs)
            self.assertEqual(result['out/shell-alias']['target'], 'shell')
            self.assertIn(self.ENGINE, result)
        restart_fixture(self)
        with self.subTest(phase="foreign_required_executable_rejected"):
            path = self.root / 'out/shell'
            data = bytearray(path.read_bytes())
            struct.pack_into('<H', data, 18, 62)
            path.write_bytes(data)
            with self.assertRaisesRegex(ValueError, 'foreign'):
                artifact.inventory(self.root, self.inputs)
        restart_fixture(self)
        with self.subTest(phase="missing_browser_engine_rejected"):
            (self.root / self.ENGINE).unlink()
            with self.assertRaisesRegex(ValueError, 'chromium'):
                artifact.inventory(self.root, self.inputs)
        restart_fixture(self)
        with self.subTest(phase="security_tools_are_required"):
            for name in ('tilo-crypto', 'tilo-image-trust', 'tilo-device-key-init'):
                with self.subTest(name=name):
                    path = self.root / 'out' / name
                    data = path.read_bytes()
                    path.unlink()
                    with self.assertRaisesRegex(ValueError, name):
                        artifact.inventory(self.root, self.inputs)
                    self.write('out/' + name, data, 0o755)
        restart_fixture(self)
        with self.subTest(phase="unexpected_root_rejected"):
            self.write('etc/auth.json', b'{}')
            with self.assertRaisesRegex(ValueError, 'unexpected'):
                artifact.inventory(self.root, self.inputs)

    def test_secrets_and_special_files(self):
        with self.subTest(phase="secret_identity_and_pem_rejected"):
            for data in (b'{"secret_key_hex":"' + b'a' * 64 + b'"}',
                         b'-----BEGIN PRIVATE KEY-----\nQUFB\n-----END PRIVATE KEY-----'):
                with self.subTest(data=data):
                    self.write('out/unexpected.json', data)
                    with self.assertRaisesRegex(ValueError, 'private signing'):
                        artifact.inventory(self.root, self.inputs)
        restart_fixture(self)
        with self.subTest(phase="escaping_symlink_and_special_modes_rejected"):
            path = self.root / 'out/escape'
            path.symlink_to('/etc/passwd')
            with self.assertRaisesRegex(ValueError, 'absolute'):
                artifact.inventory(self.root, self.inputs)
            path.unlink()
            # An environment image's links are read inside an environment.
            image = self.root / 'usr/share/tilo-apps/pkgs' / ('ab' * 32) / 'etc'
            image.mkdir(parents=True)
            (image / 'mtab').symlink_to('../proc/self/mounts')
            rows = artifact.inventory(self.root, self.inputs)
            self.assertEqual(rows['usr/share/tilo-apps/pkgs/' + 'ab' * 32 + '/etc/mtab']['target'],
                             '../proc/self/mounts')
            (self.root / 'out/shell').chmod(0o4755)
            with self.assertRaisesRegex(ValueError, 'mode'):
                artifact.inventory(self.root, self.inputs)

    def test_receipt_pinning(self):
        with self.subTest(phase="pinned_metadata_does_not_hide_changed_payload"):
            rows = artifact.inventory(self.root, self.inputs)
            metadata = artifact.tools.canonical({'schema': 1, 'purpose': artifact.PURPOSE,
                                                'inputs': self.inputs, 'inventory': rows})
            self.write(artifact.METADATA, metadata)
            @contextmanager
            def mounted(_): yield self.root
            with patch.object(artifact.tools, 'mounted', mounted):
                artifact.check_payload('sha256:' + '1' * 64, self.inputs, artifact.tools.sha(metadata))
                self.write('usr/share/tilo-apps/registry.toml', b'changed')
                with self.assertRaisesRegex(ValueError, 'inventory differs'):
                    artifact.check_payload('sha256:' + '1' * 64, self.inputs, artifact.tools.sha(metadata))
        restart_fixture(self)
        with self.subTest(phase="receipt_hash_failure_precedes_tools"):
            path = self.root / 'receipt.json'
            path.write_text('{}')
            with patch.object(artifact.tools, 'command') as command:
                with self.assertRaisesRegex(ValueError, 'receipt hash'):
                    artifact.import_artifact(path, '0' * 64, self.inputs)
                command.assert_not_called()
        restart_fixture(self)
        with self.subTest(phase="source_mismatch_rejected_before_export"):
            with patch.object(artifact.tools, 'command', return_value=b'b' * 40):
                with self.assertRaisesRegex(ValueError, 'source revision'):
                    artifact.expected_inputs(self.root, 'a' * 40, 'aarch64', '', '', 1)
        restart_fixture(self)
        with self.subTest(phase="valid_pinned_receipt_rejects_changed_build_inputs"):
            receipt = {'schema': 1, 'purpose': artifact.PURPOSE, 'inputs': self.inputs,
                       'reference': artifact.repository('aarch64') + '@sha256:' + '1' * 64,
                       'config_digest': 'sha256:' + '2' * 64, 'metadata_sha256': '3' * 64,
                       'source_payload_id': 'sha256:' + '4' * 64, 'run_id': '1', 'attempt': '1',
                       'production_signed': False}
            raw = artifact.tools.canonical(receipt)
            path = self.root / 'receipt.json'
            path.write_bytes(raw)
            with patch.object(artifact.tools, 'command') as command:
                with self.assertRaisesRegex(ValueError, 'inputs differ'):
                    artifact.import_artifact(path, artifact.tools.sha(raw), self.inputs | {'revision': 'b' * 40})
                command.assert_not_called()


if __name__ == '__main__':
    unittest.main()
