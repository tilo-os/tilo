"""Actual owned-child lifecycle boundaries; these do not qualify a GPT source."""
from pathlib import Path
import subprocess
import sys
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / 'tools/tests'))
from hang import HANG_BOUND  # noqa: E402

import bounded_native as native


class OwnedChildBoundaries(unittest.TestCase):
    def test_actual_exit_status_survives_cleanup(self):
        self.assertEqual(native.bounded(
            [sys.executable, '-c', 'print("owned"); raise SystemExit(7)'], timeout=HANG_BOUND,
        ), (7, b'owned\n', b''))
        native.assert_children_retired()

    def test_deadline_reaps_actual_child(self):
        with self.assertRaisesRegex(RuntimeError, 'deadline exceeded'):
            native.bounded([sys.executable, '-c', 'import time; time.sleep(3600)'], timeout=.1)
        native.assert_children_retired()

    def test_output_cap_reaps_actual_child(self):
        with self.assertRaisesRegex(RuntimeError, 'output exceeded bound'):
            native.bounded([sys.executable, '-c', 'print("x"*256)'], limit=32, timeout=HANG_BOUND)
        native.assert_children_retired()

    def test_selector_setup_failure_reaps_already_spawned_child(self):
        children = []
        real = native.subprocess.Popen

        def track(*args, **kwargs):
            child = real(*args, **kwargs)
            children.append(child)
            return child

        with mock.patch.object(native.subprocess, 'Popen', track), mock.patch.object(
            native.selectors, 'DefaultSelector', side_effect=OSError('selector allocation refused'),
        ):
            with self.assertRaisesRegex(OSError, 'allocation refused'):
                native.bounded([sys.executable, '-c', 'import time; time.sleep(3600)'], timeout=1)
        self.assertEqual(len(children), 1)
        self.assertIsNotNone(children[0].returncode)
        native.assert_children_retired()

    def test_unknown_wait_retains_actual_child_and_blocks_fixture_release(self):
        child = subprocess.Popen(
            [sys.executable, '-c', 'import time; time.sleep(3600)'], start_new_session=True,
        )
        try:
            with mock.patch.object(child, 'wait', side_effect=subprocess.TimeoutExpired(child.args, .01)):
                with self.assertRaisesRegex(RuntimeError, 'retirement unknown'):
                    native.retire_child(child, timeout=.01)
            self.assertIs(native._OUTSTANDING[id(child)], child)
            with self.assertRaisesRegex(RuntimeError, 'retirement unknown'):
                native.assert_children_retired()
            child.kill()
            child.wait(timeout=HANG_BOUND)
            del native._OUTSTANDING[id(child)]
            native.assert_children_retired()
        finally:
            if child.returncode is None:
                native.retire_child(child)


if __name__ == '__main__':
    unittest.main()
