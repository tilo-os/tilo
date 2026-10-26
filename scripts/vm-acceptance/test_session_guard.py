"""No check may hand the next one a slot whose session does not answer.

A17 ends the session on purpose; when the session after it died too, every later check
failed on a missing control socket until something brought it back. These run `run_check`
against a fake VM whose session can stop, come back by itself or need restarting (no
running VM):

    python3 -m unittest scripts/vm-acceptance/test_session_guard.py
"""

import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))

from acceptance import core  # noqa: E402
from acceptance.context import Context  # noqa: E402
from acceptance.vm import VmError  # noqa: E402

STOPPED = "error: tilo-control failed: the slot's session is not running"


class StoppingVm:
    """A slot whose session answers while `running`; `returns_after` state reads of a stopped
    session bring it back by itself (systemd's Restart=), None never."""

    slot = "acceptance"

    def __init__(self, returns_after=None):
        self.running = True
        self.returns_after = returns_after
        self.reads_while_stopped = 0
        self.calls = []

    def state(self):
        if not self.running:
            self.reads_while_stopped += 1
            if self.returns_after is not None and self.reads_while_stopped >= self.returns_after:
                self.running = True
            else:
                raise VmError(STOPPED)
        return {"chrome": {"session_locked": False, "overlays": {}, "palette": None,
                           "window_switcher": {"open": False}}}

    def restart_session(self):
        self.calls.append(("restart_session",))
        self.running = True

    def root_exec(self, script, timeout=120):
        return ""


def fast_wait_until(predicate, timeout, interval=0.5):
    """wait_until without the wall-clock wait: a few reads stand for the timeout."""
    for _ in range(5):
        value = predicate()
        if value:
            return value
    return None


class SessionGuardTest(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.out = Path(tmp.name)
        p = patch.object(core, "wait_until", fast_wait_until)
        p.start()
        self.addCleanup(p.stop)

    def run_stopping(self, vm):
        ctx = Context(vm, self.out, {})

        def stops(c, r):
            c.vm.running = False

        return ctx, core.run_check(ctx, core.Check("X10", "apps", "stops the session", stops))

    def test_a_session_that_comes_back_by_itself_is_waited_for(self):
        vm = StoppingVm(returns_after=3)
        ctx, res = self.run_stopping(vm)
        self.assertEqual(res.status, "pass", res.detail)
        self.assertNotIn(("restart_session",), vm.calls)
        self.assertNotIn("session_recovered_after", ctx.facts)

    def test_a_session_that_stays_down_is_restarted_and_fails_the_check(self):
        vm = StoppingVm()
        ctx, res = self.run_stopping(vm)
        self.assertEqual(res.status, "fail")
        self.assertIn("left no slot session answering", res.detail)
        self.assertIn("the harness restarted the slot session", res.detail)
        self.assertEqual(vm.calls.count(("restart_session",)), 1)
        self.assertTrue(vm.running)
        self.assertEqual(ctx.facts["session_recovered_after"], ["X10"])

    def test_an_answering_session_is_left_alone(self):
        vm = StoppingVm()
        ctx = Context(vm, self.out, {})
        res = core.run_check(ctx, core.Check("X11", "apps", "passes", lambda c, r: None))
        self.assertEqual(res.status, "pass", res.detail)
        self.assertEqual(vm.calls, [])



class KeepAwakeTest(unittest.TestCase):
    """The person's idle period is restarted before a check once the last press is old."""

    class Vm:
        def __init__(self, fails=False):
            self.keys, self.fails = [], fails

        def key(self, *combos):
            if self.fails:
                raise VmError("error: input failed")
            self.keys.append(combos)

    class Ctx:
        def __init__(self, vm):
            self.vm, self.facts = vm, {}

    def test_presses_once_then_not_again_until_the_last_press_is_old(self):
        ctx = self.Ctx(self.Vm())
        with patch.object(core.time, "sleep"), patch.object(core.time, "monotonic", return_value=1000.0):
            core.keep_awake(ctx)
            core.keep_awake(ctx)
        self.assertEqual(ctx.vm.keys, [("shift",)])
        with patch.object(core.time, "sleep"), \
                patch.object(core.time, "monotonic", return_value=1000.0 + core.AWAKE_EVERY_S):
            core.keep_awake(ctx)
        self.assertEqual(ctx.vm.keys, [("shift",), ("shift",)])

    def test_input_that_fails_is_noted_not_raised(self):
        ctx = self.Ctx(self.Vm(fails=True))
        core.keep_awake(ctx)
        self.assertIn("input failed", ctx.facts["awake_failed"])
        self.assertNotIn("awake_at", ctx.facts)


if __name__ == "__main__":
    unittest.main()
