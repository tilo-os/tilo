"""Unit tests for the after-check window guard and windows handed on to later checks.

    python3 -m unittest discover -s scripts/vm-acceptance -p 'test_*.py'
"""

import sys
import unittest
from pathlib import Path
from types import SimpleNamespace

sys.path.insert(0, str(Path(__file__).resolve().parent))

from acceptance import core  # noqa: E402


def ctx_with(groups, kept=None):
    closed = []
    state = {"chrome": {}, "workspaces": [{"strips": [{"groups": [{"id": g, "tabs": []} for g in groups]}]}]}
    ctx = SimpleNamespace(facts={}, state=lambda: state,
                          close_all_windows=lambda: closed.append(True))
    if kept is not None:
        ctx.facts[core.KEPT_WINDOWS] = set(kept)
    return ctx, closed


class KeptWindows(unittest.TestCase):
    def setUp(self):
        self.groups = core.all_groups
        core.all_groups = lambda st: st["workspaces"][0]["strips"][0]["groups"]
        self.addCleanup(setattr, core, "all_groups", self.groups)

    def test_windows_handed_on_stay_open_for_the_next_check(self):
        ctx, closed = ctx_with([1, 2], kept=[1, 2])
        res = core.Result("W01", "window", "W01", status="pass")
        core.leave_no_stuck_windows(ctx, core.Check("W01", "window", "W01", None), res)
        self.assertEqual(closed, [])
        self.assertEqual(res.status, "pass")

    def test_a_window_beyond_those_handed_on_is_closed(self):
        ctx, closed = ctx_with([1, 2, 3], kept=[1, 2])
        res = core.Result("W02", "window", "W02", status="pass")
        core.leave_no_stuck_windows(ctx, core.Check("W02", "window", "W02", None), res)
        self.assertEqual(closed, [True])
        self.assertEqual(res.status, "pass")

    def test_without_handed_on_windows_every_window_is_closed(self):
        ctx, closed = ctx_with([5])
        res = core.Result("A05", "apps", "A05", status="pass")
        core.leave_no_stuck_windows(ctx, core.Check("A05", "apps", "A05", None), res)
        self.assertEqual(closed, [True])


if __name__ == "__main__":
    unittest.main()
