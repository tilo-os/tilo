"""Theme selection must use the shell's semantic actions, including repaint notification."""

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))

from acceptance.checks.ui import set_theme, theme_contrast_problems  # noqa: E402
from acceptance.vm import TiloVm, VmError  # noqa: E402


class ThemeActionsTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.ctx = SimpleNamespace(vm=TiloVm("tilo-vm", "test-vm", "test-slot",
                                           Path(self.tmp.name) / "commands.log"))
        self.commands = []
        self.failure = None
        self.launch = patch("acceptance.vm.subprocess.run", side_effect=self.run_command)
        self.launch.start()
        self.addCleanup(self.launch.stop)
        self.sleep = patch("acceptance.checks.ui.time.sleep")
        self.sleep.start()
        self.addCleanup(self.sleep.stop)

    def run_command(self, cmd, **kwargs):
        self.commands.append(cmd)
        if cmd[-1] == self.failure:
            return subprocess.CompletedProcess(cmd, 1, "", "appearance action refused")
        # Support the old helper's settings read so red is an assertion failure.
        stdout = "{}" if "exec" in cmd else '{"ok":true}'
        return subprocess.CompletedProcess(cmd, 0, stdout, "")

    def test_light_and_dark_use_semantic_actions_without_writing_settings(self):
        for theme, action in [("light", "appearance.theme.light"),
                              ("dark", "appearance.theme.dark")]:
            with self.subTest(theme=theme):
                self.commands.clear()
                set_theme(self.ctx, theme)
                self.assertEqual(self.commands, [
                    ["tilo-vm", "control", "actions", "invoke", "actions", action],
                ])

    def test_high_contrast_selects_dark_before_enabling_contrast(self):
        set_theme(self.ctx, "high-contrast")
        self.assertEqual(self.commands, [
            ["tilo-vm", "control", "actions", "invoke", "actions", "appearance.theme.dark"],
            ["tilo-vm", "control", "actions", "invoke", "settings-pages",
             "setting:appearance.high_contrast"],
        ])

    def test_dark_refusal_stops_high_contrast_selection(self):
        self.failure = "appearance.theme.dark"
        with self.assertRaisesRegex(VmError, "appearance action refused"):
            set_theme(self.ctx, "high-contrast")
        self.assertEqual(self.commands, [
            ["tilo-vm", "control", "actions", "invoke", "actions", "appearance.theme.dark"],
        ])

    def test_action_refusals_reach_the_acceptance_check(self):
        for theme, action in [("light", "appearance.theme.light"),
                              ("dark", "appearance.theme.dark"),
                              ("high-contrast", "setting:appearance.high_contrast")]:
            with self.subTest(theme=theme):
                self.failure = action
                with self.assertRaisesRegex(VmError, "appearance action refused"):
                    set_theme(self.ctx, theme)


class ThemeContrastTest(unittest.TestCase):
    def test_a_theme_that_reached_the_chrome_passes(self):
        # Run 97408: light 0.979, dark 0.110.
        self.assertEqual(theme_contrast_problems({"light": 0.979, "dark": 0.11, "high-contrast": 0.015}), [])

    def test_a_theme_change_that_did_not_reach_the_chrome_fails(self):
        # The old settings-file write: both passes drew the dark default.
        [problem] = theme_contrast_problems({"light": 0.101, "dark": 0.101, "high-contrast": 0.015})
        self.assertIn("look alike (luminance 0.101 vs 0.101)", problem)
        [problem] = theme_contrast_problems({"light": 0.2, "dark": 0.11})
        self.assertIn("look alike", problem)

    def test_an_unmeasured_pass_fails_instead_of_passing(self):
        [problem] = theme_contrast_problems({"dark": 0.11})
        self.assertIn("not measured in light", problem)


if __name__ == "__main__":
    unittest.main()
