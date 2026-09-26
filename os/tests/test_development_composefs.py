"""Storage digest identity/version boundaries; no image or disk operation."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

DEV = Path(__file__).resolve().parents[1] / 'dev'
spec = importlib.util.spec_from_file_location('digest', DEV / 'compute-development-composefs.py')
digest = importlib.util.module_from_spec(spec)
spec.loader.exec_module(digest)
IMAGE = 'sha256:' + '1' * 64


class DigestTests(unittest.TestCase):
    def test_composefs_digest_inputs(self):
        with self.subTest(phase="exact_digest_only"):
            for value in ('a' * 128, 'a' * 128 + '\n'):
                self.assertEqual(digest.parse_digest(value), 'a' * 128)
            for value in ('0' * 128, 'A' * 128, 'a' * 127, ' a' * 128,
                          'log\n' + 'a' * 128, 'a' * 128 + '\n\n', 'sha512:' + 'a' * 128):
                with self.subTest(value=value), self.assertRaises(ValueError):
                    digest.parse_digest(value)
        with self.subTest(phase="identity_and_architecture"):
            valid = {'Id': IMAGE, 'Os': 'linux', 'Architecture': 'arm64'}
            digest.validate_identity(json.dumps([valid]), IMAGE, 'aarch64')
            for changes in ({'Id': 'sha256:' + '2' * 64}, {'Os': 'windows'}, {'Architecture': 'amd64'}):
                with self.subTest(changes=changes), self.assertRaises(ValueError):
                    digest.validate_identity(json.dumps([valid | changes]), IMAGE, 'aarch64')
        with self.subTest(phase="wrong_version_stops_before_image_access"):
            with patch.object(digest, 'host_inputs'), patch.object(digest, 'command', return_value='bootc 1.16.3\n') as command:
                with self.assertRaisesRegex(ValueError, 'exactly version'):
                    digest.compute(IMAGE, 'aarch64')
                self.assertEqual(command.call_count, 1)
        with self.subTest(phase="immutable_id_used_for_computation"):
            with tempfile.NamedTemporaryFile() as binary:
                binary.write(b'tool fixture'); binary.flush()
                replies = ['bootc 1.16.4\n', json.dumps([{'Id': IMAGE, 'Os': 'linux', 'Architecture': 'arm64'}]), 'b' * 128 + '\n']
                with patch.object(digest, 'host_inputs'), patch.object(digest, 'BOOTC', binary.name), patch.object(digest, 'command', side_effect=replies) as command:
                    receipt = digest.compute(IMAGE, 'aarch64')
                self.assertEqual(command.call_args.args[0], [binary.name, 'container', 'compute-composefs-digest-from-storage', IMAGE])
                self.assertEqual(receipt['image_id'], IMAGE)
                self.assertFalse(receipt['installation_verified'])
        with self.subTest(phase="mutable_reference_rejected_before_host_access"):
            with self.assertRaisesRegex(ValueError, 'immutable'):
                digest.host_inputs('localhost/tilo:latest', 'aarch64')


if __name__ == '__main__':
    unittest.main()
