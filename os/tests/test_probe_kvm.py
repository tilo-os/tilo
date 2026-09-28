#!/usr/bin/env python3
"""Exercise the KVM capability decision without requiring a KVM host."""

import errno
import importlib.util
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

if sys.platform.startswith('linux'):
    SOURCE = Path(__file__).resolve().parents[1] / 'dev/probe-kvm.py'
    spec = importlib.util.spec_from_file_location('probe_kvm', SOURCE)
    probe = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(probe)
else:
    probe = None


@unittest.skipUnless(sys.platform.startswith('linux'), 'KVM ioctls require Linux')
class ProbeKvmTests(unittest.TestCase):
    def test_architecture_mismatch_skips_device(self):
        with patch.object(probe.platform, 'machine', return_value='aarch64'), \
             patch.object(probe.os, 'open') as open_device:
            self.assertFalse(probe.available('x86_64'))
            open_device.assert_not_called()

    def test_unusable_device_falls_back(self):
        with patch.object(probe.platform, 'machine', return_value='aarch64'), \
             patch.object(probe.os, 'open', return_value=10), \
             patch.object(probe.os, 'close') as close, \
             patch.object(probe.fcntl, 'ioctl', side_effect=[12, OSError(errno.ENODEV, 'nested KVM denied')]):
            self.assertFalse(probe.available('arm64'))
            close.assert_called_once_with(10)

    def test_creatable_vm_enables_kvm(self):
        with patch.object(probe.platform, 'machine', return_value='aarch64'), \
             patch.object(probe.os, 'open', return_value=10), \
             patch.object(probe.os, 'close') as close, \
             patch.object(probe.fcntl, 'ioctl', side_effect=[12, 11]):
            self.assertTrue(probe.available('aarch64'))
            self.assertEqual([call.args[0] for call in close.call_args_list], [11, 10])


if __name__ == '__main__':
    unittest.main()
