"""Unit tests for the secret-custody checks' parsing and first-use steps (no VM).

    python3 -m unittest discover -s scripts/vm-acceptance -p 'test_*.py'
"""

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from acceptance.checks import secrets  # noqa: E402
from acceptance.core import CheckFailed  # noqa: E402


class FirstUseSteps(unittest.TestCase):
    def test_an_acknowledgement_comes_before_going_forward_and_only_once(self):
        offered = {"onboarding.understood", "onboarding.back", "onboarding.create"}
        self.assertEqual(secrets.next_first_use_step(offered, set()), "onboarding.understood")
        self.assertEqual(secrets.next_first_use_step(offered, {"onboarding.understood"}),
                         "onboarding.create")

    def test_a_backup_is_declined_then_continued(self):
        offered = {"onboarding.backup.skip-acknowledged"}
        self.assertEqual(secrets.next_first_use_step(offered, set()),
                         "onboarding.backup.skip-acknowledged")
        offered |= {"onboarding.backup.continue"}
        self.assertEqual(secrets.next_first_use_step(offered, {"onboarding.backup.skip-acknowledged"}),
                         "onboarding.backup.continue")

    def test_the_intro_begins_rather_than_restoring_and_the_tour_is_skipped(self):
        self.assertEqual(secrets.next_first_use_step({"onboarding.begin", "onboarding.existing"}, set()),
                         "onboarding.begin")
        self.assertEqual(secrets.next_first_use_step(
            {"onboarding.tour.back", "onboarding.tour.skip", "onboarding.tour.next"}, set()),
            "onboarding.tour.skip")

    def test_nothing_known_is_none(self):
        self.assertIsNone(secrets.next_first_use_step({"onboarding.existing.nearby"}, set()))


class Parsing(unittest.TestCase):
    def test_fields_and_sections(self):
        text = "PID=42\nLS-EXIT=2\nLS: ls: cannot read symbolic link: Permission denied\n" \
               "STATUS-BEGIN\nopen\nSTATUS-END\n"
        self.assertEqual(secrets.fields(text)["PID"], "42")
        self.assertEqual(secrets.fields(text)["LS-EXIT"], "2")
        self.assertEqual(secrets.between(text, "STATUS-BEGIN", "STATUS-END"), "open")
        self.assertEqual(secrets.between(text, "APPS-BEGIN", "APPS-END"), "")


class CompositorRow(unittest.TestCase):
    ROW = {"id": secrets.COMPOSITOR_APP, "access": "allowed", "items": 2, "uses": 5}

    def test_the_compositor_with_items_passes(self):
        self.assertEqual(secrets.judge_compositor_row({secrets.COMPOSITOR_APP: self.ROW}, "x"), self.ROW)

    def test_missing_refused_or_empty_fails(self):
        with self.assertRaises(CheckFailed):
            secrets.judge_compositor_row({}, "x")
        with self.assertRaises(CheckFailed):
            secrets.judge_compositor_row({secrets.COMPOSITOR_APP: dict(self.ROW, access="revoked")}, "x")
        with self.assertRaises(CheckFailed):
            secrets.judge_compositor_row({secrets.COMPOSITOR_APP: self.ROW}, "x", min_items=3)


if __name__ == "__main__":
    unittest.main()
