"""Local real SQLite/flock preparation; not Kubernetes/PVC acceptance evidence."""
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import controller_runtime_probe as probe


class RuntimeProbePreparationTests(unittest.TestCase):
    def test_actual_reviewed_source_hashes_match(self):
        self.assertEqual(probe.SOURCE_HASHES, probe.checked_source())

    def test_source_hashes_follow_configmap_projection_symlinks(self):
        source = Path(probe.__file__).parent
        with tempfile.TemporaryDirectory() as directory:
            mount = Path(directory) / 'tools'
            timestamp = mount / '..timestamp'
            timestamp.mkdir(parents=True)
            for name in probe.SOURCE_HASHES:
                relative = name.removeprefix('tools/')
                destination = timestamp / relative
                destination.parent.mkdir(parents=True, exist_ok=True)
                destination.write_bytes((source / relative).read_bytes())
            (timestamp / 'controller_runtime_probe.py').write_bytes(Path(probe.__file__).read_bytes())
            link = mount / 'controller_runtime_probe.py'
            link.symlink_to(timestamp / link.name)
            with patch.object(probe, '__file__', str(link)):
                self.assertEqual(probe.SOURCE_HASHES, probe.checked_source())

    def test_committed_crash_intent_reopens_in_another_process_and_cleans_only_fixture(self):
        with tempfile.TemporaryDirectory() as directory:
            state = Path(directory) / 'runtime-qualification.sqlite'
            written = probe.write_phase(state)
            self.assertTrue(written['cross_process_nonblocking_lock_rejection'])
            self.assertEqual(73, written['simulated_dispatch_exit'])
            with self.assertRaisesRegex(ValueError, 'differs from write Pod'):
                probe.restart_phase(state, 'wrong-intent')
            self.assertTrue(state.is_file())
            def restart():
                result = probe.restart_phase(state, written['intent_id'])
                if not result['no_api_access'] or not result['service_dry_run_same_state']:
                    os._exit(4)
                os._exit(0)
            probe.child(restart, 0)
            self.assertEqual([], list(Path(directory).iterdir()))

    def test_real_state_and_fixture_links_are_never_opened(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            real = root / 'coordinator.sqlite'
            real.write_bytes(b'real state sentinel')
            with self.assertRaisesRegex(ValueError, 'real coordinator'):
                probe.write_phase(root / 'runtime-qualification.sqlite')
            self.assertEqual(b'real state sentinel', real.read_bytes())
            self.assertEqual([real], list(root.iterdir()))
            with self.assertRaisesRegex(ValueError, 'only fixture'):
                probe.write_phase(real)
            real.unlink()
            target = root / 'outside'
            target.write_bytes(b'untouched')
            fixture = root / 'runtime-qualification.sqlite'
            fixture.symlink_to(target)
            with self.assertRaisesRegex(ValueError, 'links refused'):
                probe.write_phase(fixture)
            self.assertEqual(b'untouched', target.read_bytes())


if __name__ == '__main__':
    unittest.main()
