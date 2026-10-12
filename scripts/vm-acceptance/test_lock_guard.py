"""No check may leave the shared session locked, and W21 never pictures a locked one.

W21 locks the session on purpose. While locked, the compositor refuses every snapshot and
action with `session_locked`, so a W21 that stopped half-way used to fail every app check
after it. These run W21 and `run_check` against a fake VM that keeps a lock the way the
compositor does (no running VM):

    python3 -m unittest scripts/vm-acceptance/test_lock_guard.py
"""

import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))

from acceptance import core  # noqa: E402
from acceptance.checks import window  # noqa: E402
from acceptance.context import Context  # noqa: E402
from acceptance.vm import VmError  # noqa: E402

PASSCODE = "touch7"
LOCKED = "error [session_locked]: the session is locked; unlock it at the device, then retry"


class LockingVm:
    """The parts of TiloVm W21 and run_check use, over a lock that behaves like the
    compositor's: snapshots refused while locked, a touch wakes the lock's keyboard,
    its keys and the real keyboard type into the field, Return checks the passcode."""

    def __init__(self, *, fail_touch=False, keyboard_unlocks=True, snapshot_leaks=False):
        self.fail_touch = fail_touch
        self.keyboard_unlocks = keyboard_unlocks
        self.snapshot_leaks = snapshot_leaks
        self.locked = False
        self.keyboard = False
        self.field = ""
        self.calls = []
        letters = "abcdefghijklmnopqrstuvwxyz0123456789"
        self.keys = [{"key": c, "x": 10 * i, "y": 500, "w": 10, "h": 10} for i, c in enumerate(letters)]
        self.keys.append({"key": "enter", "x": 10 * len(letters), "y": 500, "w": 10, "h": 10})

    # -- what the session shows ------------------------------------------------

    def state(self):
        lock_auth = ({"password_len": len(self.field), "keyboard": self.keys if self.keyboard else None}
                     if self.locked else None)
        return {"chrome": {"session_locked": self.locked, "lock_auth": lock_auth, "overlays": {},
                           "palette": None, "window_switcher": {"open": False}},
                "workspace_tree": {"outputs": {"displays": [
                    {"id": 0, "x": 0, "y": 0, "width": 1280, "height": 800}]}}}

    def actions(self, query=""):
        return []

    # -- input -------------------------------------------------------------------

    def _submit(self, unlocks):
        if self.field == PASSCODE and unlocks:
            self.locked, self.keyboard = False, False
        self.field = ""

    def key(self, *combos):
        self.calls.append(("key", *combos))
        for combo in combos:
            if combo == "super+l":
                self.locked = True
            elif self.locked and combo == "Escape":
                self.field = ""
            elif self.locked and combo == "Return":
                self._submit(self.keyboard_unlocks)

    def input(self, *args):
        self.calls.append(("input", *args))
        if args[0] == "type":
            if self.locked:
                self.field += args[1]
            return ""
        if args[:2] == ("touch", "tap"):
            if self.fail_touch:
                raise VmError("`tilo-vm input touch tap` exited 1: no touch device on the seat")
            if self.locked and not self.keyboard:
                self.keyboard = True
            elif self.locked:
                x, y = int(args[2]), int(args[3])
                k = next(k for k in self.keys
                         if k["x"] <= x < k["x"] + k["w"] and k["y"] <= y < k["y"] + k["h"])
                if k["key"] == "enter":
                    self._submit(True)
                else:
                    self.field += k["key"]
        return ""

    # -- pictures --------------------------------------------------------------

    def _picture(self, kind, path):
        self.calls.append((kind, self.locked, str(path)))
        if self.locked and not self.snapshot_leaks:
            raise VmError(f"`tilo-vm shot -o {path}` exited 1: error: tilo-control snapshot failed: {LOCKED}")
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(b"\x89PNG")
        return path

    def shot(self, path):
        return self._picture("shot", path)

    def snapshot_once(self, path):
        return self._picture("snapshot_once", path)

    # -- the slot ------------------------------------------------------------------

    def restart_session(self):
        self.calls.append(("restart_session",))
        self.locked, self.keyboard, self.field = False, False, ""

    def slot_uid(self):
        return 1001

    def root_exec(self, script, timeout=120):
        self.calls.append(("root_exec", script))
        return ""


def fast_wait_until(predicate, timeout, interval=0.5):
    """wait_until without the wall-clock wait: the fake's state changes only on input."""
    for _ in range(3):
        value = predicate()
        if value:
            return value
    return None


class LockGuardTest(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.out = Path(tmp.name)
        for target in ("acceptance.context.time.sleep", "acceptance.vm.time.sleep"):
            p = patch(target, lambda _s: None)
            p.start()
            self.addCleanup(p.stop)
        p = patch.object(window, "wait_until", fast_wait_until)
        p.start()
        self.addCleanup(p.stop)

    def run_w21(self, vm):
        ctx = Context(vm, self.out, {})
        w21 = next(c for c in core.REGISTRY if c.id == "W21")
        return ctx, core.run_check(ctx, w21)

    def pictures_while_locked(self, vm):
        return [c for c in vm.calls if c[0] in ("shot", "snapshot_once") and c[1]]

    def test_w21_passes_and_asserts_the_lock_refuses_a_snapshot(self):
        vm = LockingVm()
        _, res = self.run_w21(vm)
        self.assertEqual(res.status, "pass", res.detail)
        self.assertEqual(res.metrics["unlock_recovery"], "none")
        self.assertEqual(res.metrics["keys"], len(vm.keys))
        self.assertFalse(vm.locked)
        self.assertNotIn(("restart_session",), vm.calls)
        # The passcode went in by touch on the lock's own keyboard, not the real keyboard.
        self.assertNotIn(("input", "type", PASSCODE), vm.calls)
        self.assertIn("data/W21-lock-keyboard.json", res.evidence)
        self.assertIn("shots/W21-unlocked.png", res.evidence)
        self.assertFalse((self.out / "scratch" / "W21-locked-snapshot.png").exists())

    def test_w21_takes_no_snapshot_while_locked_but_the_refused_probe(self):
        vm = LockingVm()
        _, res = self.run_w21(vm)
        locked = self.pictures_while_locked(vm)
        self.assertEqual([c[0] for c in locked], ["snapshot_once"])
        self.assertTrue(locked[0][2].endswith("scratch/W21-locked-snapshot.png"))
        # The probe never becomes a picture in the report.
        self.assertTrue(all("W21-locked-snapshot" not in e for e in res.evidence))

    def test_w21_fails_when_the_lock_lets_a_snapshot_through(self):
        vm = LockingVm(snapshot_leaks=True)
        _, res = self.run_w21(vm)
        self.assertEqual(res.status, "fail")
        self.assertIn("snapshot succeeded while the session was locked", res.detail)
        self.assertFalse((self.out / "scratch" / "W21-locked-snapshot.png").exists())
        self.assertFalse(vm.locked, "W21 left the session locked")
        self.assertEqual(res.metrics["unlock_recovery"], "passcode typed on the real keyboard")

    def test_w21_unlocks_in_finally_when_body_fails(self):
        vm = LockingVm(fail_touch=True)
        ctx, res = self.run_w21(vm)
        self.assertEqual(res.status, "fail")
        # The failure that stopped W21 is the one reported, not the cleanup.
        self.assertIn("no touch device on the seat", res.detail)
        self.assertNotIn(core.LEFT_LOCKED, res.detail)
        after_lock = vm.calls[vm.calls.index(("key", "super+l")):]
        self.assertIn(("key", "Escape"), after_lock)
        typed = after_lock.index(("input", "type", PASSCODE))
        self.assertEqual(after_lock[typed + 1], ("key", "Return"))
        self.assertEqual(res.metrics["unlock_recovery"], "passcode typed on the real keyboard")
        self.assertNotIn(("restart_session",), vm.calls)
        self.assertFalse(vm.locked)
        self.assertNotIn("session_recovered_after", ctx.facts)

    def test_w21_falls_back_to_slot_restart(self):
        vm = LockingVm(fail_touch=True, keyboard_unlocks=False)
        ctx, res = self.run_w21(vm)
        self.assertEqual(res.status, "fail")
        self.assertIn("no touch device on the seat", res.detail)
        self.assertIn(("input", "type", PASSCODE), vm.calls)
        self.assertEqual(vm.calls.count(("restart_session",)), 1)
        self.assertEqual(res.metrics["unlock_recovery"], "slot session restarted")
        self.assertFalse(vm.locked)
        self.assertNotIn("session_recovered_after", ctx.facts)

    def test_run_check_fails_a_check_that_leaves_session_locked(self):
        vm = LockingVm()
        ctx = Context(vm, self.out, {})
        res = core.run_check(ctx, core.Check("X01", "window", "locks and passes",
                                             lambda c, r: c.vm.key("super+l")))
        self.assertEqual(res.status, "fail")
        self.assertIn("left the session locked; the harness restarted the slot session", res.detail)
        self.assertEqual(vm.calls.count(("restart_session",)), 1)
        self.assertFalse(vm.locked)
        self.assertEqual(ctx.facts["session_recovered_after"], ["X01"])

    def test_run_check_keeps_the_checks_own_failure_beside_the_lock(self):
        def locks_then_fails(c, r):
            c.vm.key("super+l")
            raise core.CheckFailed("the subject was wrong")

        vm = LockingVm()
        ctx = Context(vm, self.out, {})
        res = core.run_check(ctx, core.Check("X02", "window", "locks and fails", locks_then_fails))
        self.assertEqual(res.status, "fail")
        self.assertTrue(res.detail.startswith("the subject was wrong; left the session locked"), res.detail)
        self.assertFalse(vm.locked)

    def test_run_check_reports_a_restart_that_fails(self):
        vm = LockingVm()

        def broken_restart():
            vm.calls.append(("restart_session",))
            raise VmError("`tilo-vm slot restart acceptance` exited 1: error: the slot is gone")

        vm.restart_session = broken_restart
        ctx = Context(vm, self.out, {})
        res = core.run_check(ctx, core.Check("X03", "window", "locks", lambda c, r: c.vm.key("super+l")))
        self.assertEqual(res.status, "fail")
        self.assertIn("left the session locked, and restarting the slot session failed", res.detail)
        self.assertEqual(ctx.facts["session_recovered_after"], ["X03"])

    def test_run_check_leaves_an_unlocked_session_alone(self):
        vm = LockingVm()
        ctx = Context(vm, self.out, {})
        res = core.run_check(ctx, core.Check("X04", "window", "does nothing", lambda c, r: None))
        self.assertEqual(res.status, "pass", res.detail)
        self.assertNotIn(("restart_session",), vm.calls)
        self.assertNotIn("session_recovered_after", ctx.facts)


if __name__ == "__main__":
    unittest.main()
