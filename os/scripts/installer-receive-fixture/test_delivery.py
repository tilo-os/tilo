"""Production delivery command boundaries; mocked authentication is not proof."""
import importlib.util
import json
from pathlib import Path
from types import SimpleNamespace
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location('delivery', Path(__file__).with_name('delivery.py'))
delivery = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(delivery)


class DeliveryCommands(unittest.TestCase):
    def test_authenticated_custody_uses_native_cli_without_python_wrapper(self):
        with tempfile.TemporaryDirectory() as scratch:
            source = Path(scratch) / 'source'
            source.mkdir(mode=0o700)
            target = Path(scratch) / 'target'
            account = SimpleNamespace(pw_uid=1000, pw_dir='/var/home/alice')
            reviewed = {'retained_home': True, 'pending_computer_applications_scope': {'schema': 1},
                        'install_profile': {'username': 'alice', 'full_name': 'Alice'}}
            authenticated = json.dumps(reviewed).encode()
            calls = []

            def fixed(command, log, secret=None, maximum=81920):
                calls.append((command, secret))
                if command[0] == '/usr/bin/openssl':
                    return b'$6$fixture\n'
                return authenticated

            with patch.object(delivery.os, 'geteuid', return_value=0), \
                    patch.object(delivery, 'fixed', side_effect=fixed), \
                    patch.object(delivery, 'private_write'), \
                    patch.object(delivery.pwd, 'getpwnam', side_effect=[KeyError(), account]), \
                    patch.object(delivery.pwd, 'getpwuid', side_effect=KeyError()), \
                    patch.object(delivery.tempfile, 'TemporaryDirectory') as quarantine:
                quarantine.return_value.__enter__.return_value = '/owned/quarantine'
                result, actual_account = delivery.prepare(source, b'password', target, None)
            self.assertEqual(result, reviewed)
            self.assertIs(actual_account, account)
            extractor = '/usr/libexec/tilo-installer-prepared-setup-extract'
            self.assertEqual([command for command, _ in calls if command[0] == extractor], [
                [extractor, 'preflight-authenticated', '--source-dir', str(source)],
                [extractor, 'retain', '--source-dir', str(source), '--target-var', str(target)],
                [extractor, 'retain-authenticated-binding', '--source-dir', str(source),
                 '--target-var', str(target)]])
            self.assertFalse(any('python' in argument or argument.endswith('.py')
                                 for command, _ in calls for argument in command))
            self.assertEqual(calls[0][1], b'password')
            self.assertEqual(calls[1][1], authenticated)
            binding = next(secret for command, secret in calls
                           if command[:2] == [extractor, 'retain-authenticated-binding'])
            self.assertEqual(binding, authenticated)


if __name__ == '__main__':
    unittest.main()
