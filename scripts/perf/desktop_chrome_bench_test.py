"""The chrome benchmark fails when it cannot observe its subject."""
import argparse
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch

import desktop_chrome_bench as bench


class DesktopChromeBenchTests(unittest.TestCase):
    def test_observation_succeeds_without_waiting(self):
        proc = Mock()
        bench.wait_for(lambda: True, "state", proc)
        proc.poll.assert_not_called()

    def test_dead_compositor_is_a_failure(self):
        proc = Mock()
        proc.poll.return_value = 1
        with self.assertRaisesRegex(RuntimeError, "compositor exited"):
            bench.wait_for(lambda: False, "state", proc)

    def test_unobserved_state_is_a_failure(self):
        with patch.object(bench.time, "monotonic", side_effect=[0, 20]):
            with self.assertRaisesRegex(RuntimeError, "timed out"):
                bench.wait_for(lambda: False, "state", Mock(), timeout=1)

    def test_measures_the_root_not_an_oversized_scroll_canvas(self):
        reply = {"targets": [
            {"name": "ShellOverlayHost", "rect": [0, 0, 1280, 720]},
            {"name": "ShellOverlayHost[0]/Canvas", "rect": [0, 0, 8192, 720]},
        ]}
        self.assertEqual(bench.viewport_from_targets(reply), [1280, 720])

    def test_missing_and_empty_viewports_fail(self):
        for targets in [[], [{"name": "ShellOverlayHost", "rect": [0, 0, 0, 720]}]]:
            with self.assertRaises(RuntimeError):
                bench.viewport_from_targets({"targets": targets})

    def test_existing_results_cannot_be_overwritten(self):
        with tempfile.TemporaryDirectory() as temp:
            out = Path(temp)
            (out / "before.json").write_text("preserve this result")
            with self.assertRaises(FileExistsError):
                bench.measure(argparse.Namespace(out=out, label="before"))
            self.assertEqual((out / "before.json").read_text(), "preserve this result")


if __name__ == "__main__":
    unittest.main()
