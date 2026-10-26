"""No check may hand the next one a window that does not close.

After A05 left the Store's window open, every later check failed "windows still open after
closing every group". These run `run_check` against a fake VM whose window can close, refuse
to close until its app is ended, or outlive that too (no running VM):

    python3 -m unittest scripts/vm-acceptance/test_window_guard.py
"""

import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))

from acceptance import context, core  # noqa: E402
from acceptance.context import Context  # noqa: E402


class WindowVm:
    """A desktop with one Store window: `closes` says whether its close action closes it,
    `dies_with_app` whether ending the app's processes takes the window away."""

    slot = "acceptance"

    def __init__(self, closes=True, dies_with_app=True):
        self.closes = closes
        self.dies_with_app = dies_with_app
        self.open = True
        self.calls = []

    def state(self):
        groups = [{"id": 7, "tabs": [{"tile": "so.tilo.store"}]}] if self.open else []
        return {"chrome": {"session_locked": False, "overlays": {}, "palette": None,
                           "window_switcher": {"open": False}},
                "workspace_tree": {"workspaces": [{"strips": [{"window_groups": groups}]}]}}

    def actions(self, query=""):
        return [{"provider": "desktop-windows", "id": "group.close:7", "title": "Close Store"}] if self.open else []

    def invoke(self, provider, action_id, args=None):
        self.calls.append(("invoke", action_id))
        if self.closes:
            self.open = False

    def root_exec(self, script, timeout=120):
        if script.startswith("kill -KILL"):
            self.calls.append(("kill", script))
            if self.dies_with_app:
                self.open = False
        return ""

    def restart_session(self):
        self.calls.append(("restart_session",))
        self.open = False


def fast_wait_until(predicate, timeout, interval=0.5):
    for _ in range(3):
        value = predicate()
        if value:
            return value
    return None


class WindowGuardTest(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.out = Path(tmp.name)
        p = patch.object(core, "wait_until", fast_wait_until)
        p.start()
        self.addCleanup(p.stop)
        clock = iter(range(0, 10_000, 5))
        p = patch.object(context.time, "monotonic", lambda: next(clock))
        p.start()
        self.addCleanup(p.stop)
        p = patch.object(context.time, "sleep", lambda _s: None)
        p.start()
        self.addCleanup(p.stop)
        p = patch.object(core, "processes", lambda ctx: {"procs": {
            4242: {"pid": 4242, "app": "so.tilo.store"}, 1: {"pid": 1, "app": None}}})
        p.start()
        self.addCleanup(p.stop)

    def run_leaving_a_window(self, vm):
        ctx = Context(vm, self.out, {})
        return ctx, core.run_check(ctx, core.Check("X30", "apps", "opens the Store", lambda c, r: None))

    def test_a_window_that_closes_is_closed_and_the_check_passes(self):
        vm = WindowVm()
        _, res = self.run_leaving_a_window(vm)
        self.assertEqual(res.status, "pass", res.detail)
        self.assertFalse(vm.open)
        self.assertNotIn("kill", [c[0] for c in vm.calls])

    def test_a_window_that_does_not_close_has_its_app_ended_and_fails_its_check(self):
        vm = WindowVm(closes=False)
        ctx, res = self.run_leaving_a_window(vm)
        self.assertEqual(res.status, "fail")
        self.assertIn("left a window that does not close (so.tilo.store)", res.detail)
        self.assertIn("the harness ended so.tilo.store", res.detail)
        self.assertIn(("kill", "kill -KILL 4242 2>/dev/null; true"), vm.calls)
        self.assertNotIn(("restart_session",), vm.calls)
        self.assertFalse(vm.open)

    def test_a_window_that_outlives_its_app_restarts_the_session(self):
        vm = WindowVm(closes=False, dies_with_app=False)
        ctx, res = self.run_leaving_a_window(vm)
        self.assertEqual(res.status, "fail")
        self.assertIn("the harness restarted the slot session", res.detail)
        self.assertEqual(ctx.facts["session_recovered_after"], ["X30"])
        self.assertFalse(vm.open)


if __name__ == "__main__":
    unittest.main()
