"""Unit tests for the session-restart guard: a restart of the slot's session unit fails the
check it happened in, with a dump, and P03 reads the same unit.

    python3 -m unittest discover -s scripts/vm-acceptance -p 'test_*.py'
"""

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from acceptance import core, session_unit  # noqa: E402
from acceptance.checks.provenance import session_unit_problem  # noqa: E402
from acceptance.context import Context  # noqa: E402
from acceptance.vm import VmError  # noqa: E402


class UnitVm:
    """The parts of TiloVm the guard uses: `systemctl show` and the dump over root ssh,
    and an unlocked session with no dialog for the checks' other guards."""

    slot = "acceptance"

    def __init__(self, restarts=0, readable=True):
        self.restarts = restarts
        self.readable = readable
        # systemd's exit fields: set while the main process is down, cleared once the
        # next one runs.
        self.code, self.exited = "0", ""
        self.scripts = []

    def crash(self, restarted=True):
        """The compositor segfaults; `restarted`: systemd has started the next session."""
        if restarted:
            self.restarts += 1
            self.code, self.exited = "0", ""
        else:
            self.code, self.exited = "1", f"Sun 2026-10-11 10:00:{len(self.scripts):02d} UTC"

    def restart_session(self):
        self.requested_restarts = getattr(self, "requested_restarts", 0) + 1
        self.restarts, self.code, self.exited = 0, "0", ""

    def root_exec(self, script, timeout=120):
        self.scripts.append(script)
        if not self.readable:
            raise VmError("ssh: connection refused")
        if script.startswith("systemctl show"):
            return (f"NRestarts={self.restarts}\nResult=success\nExecMainCode={self.code}\n"
                    f"ExecMainStatus={139 if self.code == '1' else 0}\nMainPID=4242\n"
                    f"ExecMainStartTimestamp=Sun 2026-10-11 10:00:04 UTC\nExecMainExitTimestamp={self.exited}\n")
        return ("=== unit\nNRestarts=1\n=== systemd on the unit\n"
                "systemd[1]: tilo-vm-session@acceptance.service: Main process exited, code=exited, status=139/n/a\n"
                "=== previous.log: panics\nthread 'main' panicked at src/x.rs:1:1")

    def state(self):
        return {"chrome": {"session_locked": False, "overlays": {}}}


class GuardTest(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.out = Path(tmp.name)

    def run_check(self, ctx, func, cid="X01"):
        return core.run_check(ctx, core.Check(cid, "apps", "a check", func))


class RestartedTest(unittest.TestCase):
    def test_a_growing_counter_is_a_restart(self):
        self.assertTrue(session_unit.restarted(0, 1))
        self.assertTrue(session_unit.restarted(2, 3))

    def test_an_unchanged_or_unreadable_counter_is_not(self):
        self.assertFalse(session_unit.restarted(1, 1))
        self.assertFalse(session_unit.restarted(None, 1))
        self.assertFalse(session_unit.restarted(0, None))

    def test_a_requested_restart_resets_the_counter_without_counting(self):
        self.assertFalse(session_unit.restarted(2, 0))
        # Reset by a requested restart, then a restart of its own.
        self.assertTrue(session_unit.restarted(2, 1))

    def test_how_it_ended_names_the_signal(self):
        fields = session_unit.parse_show("ExecMainCode=2\nExecMainStatus=6\nExecMainExitTimestamp=T\nOther=x\n")
        self.assertEqual(session_unit.how_it_ended(fields), "tilo-session was killed by signal 6 at T")
        self.assertEqual(session_unit.how_it_ended({"ExecMainCode": "1", "ExecMainStatus": "101"}),
                         "tilo-session exited with status 101 at ?")


class RunCheckTest(GuardTest):
    def test_a_restart_during_a_check_fails_it_by_name_with_the_dump(self):
        vm = UnitVm()
        ctx = Context(vm, self.out, {})

        def crashes_the_session(c, r):
            vm.crash()

        res = self.run_check(ctx, crashes_the_session, "A15")
        self.assertEqual(res.status, "fail")
        self.assertIn("the slot's desktop session restarted on its own during A15", res.detail)
        self.assertIn("code=exited, status=139/n/a", res.detail)
        self.assertIn("data/A15-1-session-restart.log", res.detail)
        self.assertIn("data/A15-1-session-restart.log", res.evidence)
        self.assertIn("panicked", (self.out / "data/A15-1-session-restart.log").read_text())
        self.assertEqual(ctx.facts["session_restarted_during"], ["A15"])

    def test_the_next_check_does_not_inherit_the_restart(self):
        vm = UnitVm()
        ctx = Context(vm, self.out, {})
        self.run_check(ctx, lambda c, r: vm.crash(), "A15")
        self.assertEqual(self.run_check(ctx, lambda c, r: None, "R04").status, "pass")

    def test_a_steady_session_passes_and_writes_nothing(self):
        vm = UnitVm(restarts=0)
        ctx = Context(vm, self.out, {})
        self.assertEqual(self.run_check(ctx, lambda c, r: None).status, "pass")
        self.assertFalse((self.out / "data").exists())

    def test_a_restart_inside_a_check_is_reported_once(self):
        vm = UnitVm()
        ctx = Context(vm, self.out, {})
        seen = []

        def notices_it_itself(c, r):
            vm.crash()
            seen.append(core.session_restart(c, "files open #3"))
            raise core.CheckFailed(seen[0])

        res = self.run_check(ctx, notices_it_itself, "A15")
        self.assertIn("during files open #3", res.detail)
        self.assertEqual(res.detail.count(core.SESSION_RESTARTED), 1)

    def test_an_ending_read_before_systemd_restarts_the_session_fails(self):
        vm = UnitVm()
        ctx = Context(vm, self.out, {})
        res = self.run_check(ctx, lambda c, r: vm.crash(restarted=False), "A15")
        self.assertIn(core.SESSION_RESTARTED, res.detail)
        # Once the next session is up the same ending is not reported again.
        vm.restarts += 1
        self.assertEqual(self.run_check(ctx, lambda c, r: None, "R04").status, "pass")

    def test_a_restart_the_harness_asked_for_passes(self):
        vm = UnitVm()
        ctx = Context(vm, self.out, {})
        self.assertEqual(self.run_check(ctx, lambda c, r: c.vm.restart_session()).status, "pass")

    def test_an_unreadable_unit_leaves_the_check_alone(self):
        ctx = Context(UnitVm(readable=False), self.out, {})
        self.assertEqual(self.run_check(ctx, lambda c, r: None).status, "pass")


class P03Test(unittest.TestCase):
    FIELDS = {"NRestarts": "0", "ExecMainCode": "1", "ExecMainStatus": "0"}

    def test_a_steady_unit_passes(self):
        self.assertIsNone(session_unit_problem(self.FIELDS, [], "acceptance"))

    def test_a_restart_fails_naming_the_unit_and_checks(self):
        why = session_unit_problem({**self.FIELDS, "NRestarts": "1", "ExecMainCode": "2",
                                    "ExecMainStatus": "11"}, ["A15"], "acceptance")
        self.assertIn("tilo-vm-session@acceptance.service 1 time(s) this boot (during A15)", why)
        self.assertIn("killed by signal 11", why)

    def test_a_restart_the_counter_lost_still_fails(self):
        # A requested restart later in the run reset NRestarts; run_check saw it happen.
        self.assertIsNotNone(session_unit_problem(self.FIELDS, ["W03"], "acceptance"))

    def test_a_restart_a_check_caused_on_purpose_passes(self):
        # A17 ends the compositor itself; systemd counts that restart too.
        self.assertIsNone(session_unit_problem({**self.FIELDS, "NRestarts": "1"}, [], "acceptance", caused=1))
        self.assertIsNotNone(session_unit_problem({**self.FIELDS, "NRestarts": "2"}, [], "acceptance", caused=1))

    def test_an_unreadable_unit_fails(self):
        self.assertIn("could not be read", session_unit_problem(None, [], "acceptance"))


if __name__ == "__main__":
    unittest.main()
