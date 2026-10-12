"""Native execution admission must reject cross-architecture/emulator claims."""
import argparse
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'android'))
from execution_host import add_architecture, native_architecture


class NativeAndroidHost(unittest.TestCase):
    def test_native_arm64_and_x86_are_admitted(self):
        with patch('execution_host.sys.platform', 'linux'):
            for architecture in ('aarch64', 'x86_64'):
                with patch('execution_host.platform.machine', return_value=architecture):
                    self.assertEqual(native_architecture(architecture), architecture)

    def test_cross_architecture_and_unsupported_hosts_fail(self):
        with patch('execution_host.sys.platform', 'linux'), patch('execution_host.platform.machine', return_value='x86_64'):
            with self.assertRaises(ValueError): native_architecture('aarch64')
            with self.assertRaises(ValueError): native_architecture('armv7')
        with patch('execution_host.sys.platform', 'darwin'):
            with self.assertRaises(ValueError): native_architecture('aarch64')

    def test_parser_defaults_to_observed_host(self):
        with patch('execution_host.platform.machine', return_value='aarch64'):
            parser = argparse.ArgumentParser()
            add_architecture(parser)
            self.assertEqual(parser.parse_args([]).architecture, 'aarch64')
            self.assertEqual(parser.parse_args(['--architecture', 'x86_64']).architecture, 'x86_64')
