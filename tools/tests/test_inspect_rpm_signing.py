"""The operator probe must never confer or execute signing authority."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('probe', ROOT / 'dist/rpm/inspect-signing-client.py')
PROBE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PROBE)


class ProbeTest(unittest.TestCase):
    def test_missing_client_is_reported_without_execution_or_authority(self):
        with tempfile.TemporaryDirectory() as directory:
            self.assertFalse(PROBE.inspect_client(Path(directory) / 'missing')['present'])
        with patch.object(PROBE, 'inspect_client', return_value={'present': False}):
            report = PROBE.report({'TILO_RPM_SIGNING_FINGERPRINT': 'A' * 40})
        self.assertEqual(report['public_fingerprint']['value'], 'A' * 40)
        for field in ('broker_executed', 'oidc_requested', 'private_key_read',
                      'backend_custody_verified', 'release_authority_qualified'):
            self.assertFalse(report[field])

    def test_symlink_is_not_read_and_malformed_variable_is_not_disclosed(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'target').write_text('must not be read')
            (root / 'client').symlink_to(root / 'target')
            result = PROBE.inspect_client(root / 'client')
            self.assertFalse(result['safe_operator_client'])
            self.assertNotIn('public_client_sha256', result)
        with patch.object(PROBE, 'inspect_client', return_value={'present': False}):
            report = PROBE.report({'TILO_RPM_SIGNING_FINGERPRINT': 'not-public-key-material'})
        self.assertIsNone(report['public_fingerprint']['value'])
        self.assertNotIn('not-public-key-material', str(report))


if __name__ == '__main__':
    unittest.main()
