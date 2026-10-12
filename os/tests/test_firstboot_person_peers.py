#!/usr/bin/env python3
"""Production firstboot transfer restores copied person-list group access."""
import importlib.util
import os
from pathlib import Path
import stat
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    'firstboot_files', ROOT / 'os/usr/libexec/tilo-firstboot-files.py')
HELPER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(HELPER)


class PersonPeersTransfer(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.peers = Path(self.temp.name)
        self.person = self.peers / ('a' * 64)
        self.person.mkdir(mode=0o700)

    def transfer(self):
        fd = os.open(self.peers, HELPER.DIR)
        try:
            HELPER.transfer_person_peers(fd, os.getuid(), os.getgid(), os.getuid())
        finally:
            os.close(fd)

    def test_private_copies_become_group_readable_and_retry_preserves_bytes(self):
        revocations = self.person / 'device-revocations'
        revocations.mkdir(mode=0o700)
        for file in [self.person / 'personal-device-peers.json',
                     self.person / 'personal-rooms.json',
                     revocations / ('a' * 64 + '-' + 'b' * 64 + '.json'),
                     self.person / 'personal-device-peers.json.lock']:
            file.write_bytes(b'preserved signed or enrollment bytes')
            file.chmod(0o600)
        # Global legacy hints are not transferred into a person's membership.
        legacy = self.peers / 'personal-device-peers.json'
        legacy.write_bytes(b'[]')
        legacy.chmod(0o640)
        for _ in range(2):
            self.transfer()
            for directory in [self.person, revocations]:
                self.assertEqual(stat.S_IMODE(directory.stat().st_mode), 0o2750)
            for file in self.person.rglob('*'):
                if file.is_file():
                    expected = 0o600 if file.name.endswith('.lock') else 0o640
                    self.assertEqual(stat.S_IMODE(file.stat().st_mode), expected)
                    self.assertEqual(file.read_bytes(), b'preserved signed or enrollment bytes')
            self.assertEqual(legacy.read_bytes(), b'[]')

    def test_symlink_and_hardlink_files_refuse_without_touching_target(self):
        target = self.peers / 'target'
        target.write_bytes(b'private')
        target.chmod(0o600)
        link = self.person / 'personal-device-peers.json'
        link.symlink_to(target)
        with self.assertRaises(ValueError):
            self.transfer()
        self.assertEqual(stat.S_IMODE(target.stat().st_mode), 0o600)
        link.unlink()
        os.link(target, link)
        with self.assertRaises(ValueError):
            self.transfer()
        self.assertEqual(stat.S_IMODE(target.stat().st_mode), 0o600)
        self.assertEqual(target.read_bytes(), b'private')

    def test_installer_repairs_legacy_lists_at_their_real_peers_location(self):
        installer = (ROOT / 'os/usr/libexec/tilo-install').read_text()
        self.assertIn('path="$HANDOFF_VAR/lib/tilo/peers/$legacy"', installer)
        self.assertIn('chmod 0640 "$path"', installer)
        self.assertNotIn('device/cert.bin personal-device-peers.json', installer)
        firstboot = (ROOT / 'os/usr/libexec/tilo-firstboot-user').read_text()
        self.assertLess(firstboot.index('"$FILES_HELPER" peers'), firstboot.index('chage -E -1'))


if __name__ == '__main__':
    unittest.main()
