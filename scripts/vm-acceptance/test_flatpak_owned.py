"""Actual acceptance selectors refuse stale, foreign and unobservable GTK custody."""
import copy
import importlib.util
import os
from pathlib import Path
import sys
import tempfile
import unittest
import uuid
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).parent))
from acceptance import host_backing
from acceptance.vm import VmError

spec = importlib.util.spec_from_file_location('flatpak_owned', Path(__file__).with_name('flatpak-owned.py'))
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


def state():
    row = {'attempt_id': str(uuid.uuid4()), 'invocation': 'own-invocation', 'adoption_id': str(uuid.uuid4()), 'room': 'room',
           'desktop_id': 'org.gnome.TextEditor.desktop', 'tab_id': 7, 'surface_id': 9,
           'peer_pid': 123, 'peer_start_time_ticks': '12345', 'launcher_pid': 122,
           'launcher_start_time_ticks': '12344'}
    return {'chrome': {'session_locked': False, 'host_window_provenance': {
        'method': 'observed-host-launch-wayland-peer', 'omitted': 0, 'windows': [row]}},
        'workspace_tree': {'workspaces': [{'room': 'room', 'strips': [{'window_groups': [{'tabs': [
            {'id': 7, 'tile': {'Foreign': {'surface_id': 9, 'app_id': 'not-trusted-for-custody'}}}]}]}]}]}}


class OwnedGtk(unittest.TestCase):
    def select(self, value, previous=()):
        return runner.subject(value, 'room', 'org.gnome.TextEditor.desktop', previous, 'own-invocation')

    def test_missing_host_projection_refuses_before_side_effects(self):
        value = state()
        value['chrome'].pop('host_window_provenance')
        with self.assertRaisesRegex(VmError, 'custody unavailable'):
            runner.host_evidence(value)

    def test_fresh_surface_uses_launch_custody_not_app_id(self):
        value = state()
        self.assertEqual(self.select(value)['peer_pid'], 123)
        other = copy.deepcopy(value['workspace_tree']['workspaces'][0]['strips'][0]['window_groups'][0]['tabs'][0])
        other['id'] = 42
        other['tile']['Foreign']['surface_id'] = 43
        value['workspace_tree']['workspaces'][0]['strips'][0]['window_groups'][0]['tabs'].append(other)
        self.assertEqual(self.select(value)['tab_id'], 7)  # Old unrelated window is retained.

    def test_previous_attempt_foreign_room_and_desktop_are_not_new_open(self):
        value = state()
        attempt = value['chrome']['host_window_provenance']['windows'][0]['attempt_id']
        self.assertIsNone(self.select(value, {attempt}))
        for field, replacement in [('room', 'foreign'), ('desktop_id', 'org.other.App.desktop'), ('invocation', 'foreign-invocation')]:
            changed = copy.deepcopy(value)
            changed['chrome']['host_window_provenance']['windows'][0][field] = replacement
            self.assertIsNone(self.select(changed))

    def test_replaced_closed_truncated_locked_or_ambiguous_refuses(self):
        for change in ('closed', 'replaced', 'truncated', 'locked', 'ambiguous', 'missing-pid'):
            value = state()
            if change == 'closed':
                value['workspace_tree']['workspaces'] = []
            elif change == 'replaced':
                value['workspace_tree']['workspaces'][0]['strips'][0]['window_groups'][0]['tabs'][0]['tile']['Foreign']['surface_id'] = 10
            elif change == 'truncated':
                value['chrome']['host_window_provenance']['omitted'] = 1
            elif change == 'locked':
                value['chrome']['session_locked'] = True
            elif change == 'missing-pid':
                value['chrome']['host_window_provenance']['windows'][0]['peer_pid'] = None
            else:
                value['chrome']['host_window_provenance']['windows'].append(copy.deepcopy(value['chrome']['host_window_provenance']['windows'][0]))
            with self.subTest(change=change), self.assertRaises(VmError):
                self.select(value)

    def test_executable_fd_matches_deployment_and_rejects_lifetime_or_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            process = root / '123'
            process.mkdir()
            uid = os.getuid() or 1002
            if os.getuid() == 0:
                os.chown(process, uid, -1)
            (process / 'stat').write_text('123 (gtk) ' + ' '.join(['1'] * 19 + ['12345']))
            resident = root / 'resident'
            resident.write_bytes(b'installed GTK executable')
            expected = root / 'installed'
            expected.write_bytes(resident.read_bytes())
            (process / 'exe').symlink_to(resident)
            proof = host_backing.probe(uid, 123, '12345', expected, root)
            self.assertEqual(proof['method'], 'proc-exe-fd')
            self.assertFalse(proof['native_sdk_claim'])
            with self.assertRaisesRegex(RuntimeError, 'owner or lifetime'):
                host_backing.probe(uid, 123, '999', expected, root)
            expected.write_bytes(b'foreign deployment')
            with self.assertRaisesRegex(RuntimeError, 'differs'):
                host_backing.probe(uid, 123, '12345', expected, root)


if __name__ == '__main__':
    unittest.main()
