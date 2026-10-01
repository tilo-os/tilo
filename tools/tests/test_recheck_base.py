"""A lander re-check narrows only its own attempt, and only when the lander asked."""
import importlib.util
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("recheck", ROOT / "tools/ci/recheck_base.py")
recheck = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(recheck)
BASE = "a" * 40


def status(description, login="qos-integrator", ident=1, context=recheck.CONTEXT):
    return {"id": ident, "context": context, "description": description, "creator": {"login": login}}


class RecheckBase(unittest.TestCase):
    def test_the_landers_status_for_this_attempt(self):
        self.assertEqual(recheck.recheck_base([status(f"{BASE} for attempt 2")], "qos-integrator", 2), BASE)

    def test_anything_else_keeps_the_plan_complete(self):
        cases = (
            ([status(f"{BASE} for attempt 2", login="someone")], 2),  # not the lander
            ([status(f"{BASE} for attempt 2")], 3),  # a later manual rerun
            ([status("b" * 12 + " for attempt 2")], 2),  # malformed
            ([status(f"{BASE} for attempt 2", context="other")], 2),
            ([], 2),
        )
        for statuses, attempt in cases:
            with self.subTest(statuses=statuses, attempt=attempt):
                self.assertIsNone(recheck.recheck_base(statuses, "qos-integrator", attempt))

    def test_the_newest_status_wins(self):
        statuses = [status(f"{BASE} for attempt 2", ident=1), status(f"{'c' * 40} for attempt 3", ident=2)]
        self.assertIsNone(recheck.recheck_base(statuses, "qos-integrator", 2))
        self.assertEqual(recheck.recheck_base(statuses, "qos-integrator", 3), "c" * 40)


if __name__ == "__main__":
    unittest.main()
