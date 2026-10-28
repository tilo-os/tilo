"""A run that reports budgets tells a check over its budget apart from one that failed.

    python3 -m unittest scripts/vm-acceptance/test_budgets.py
"""

import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace

sys.path.insert(0, str(Path(__file__).resolve().parent))

from acceptance import core, report  # noqa: E402
from acceptance.core import Result  # noqa: E402


def run(func, mode):
    ctx = SimpleNamespace(facts={}, current=None, state=lambda: {"chrome": {}}, config={"budget_mode": mode},
                          vm=SimpleNamespace(root_exec=lambda *a, **k: ""))
    return core.run_check(ctx, core.Check("R04", "resources", "Frames", func))


def slow(ctx, res):
    core.judge([], ["frame CPU p95 20 ms > 16.7 ms"])


def broken_and_slow(ctx, res):
    core.judge(["no frame painted"], ["frame CPU p95 20 ms > 16.7 ms"])


class BudgetTest(unittest.TestCase):
    def test_a_reported_budget_is_over_budget_not_a_failure(self):
        res = run(slow, "report")
        self.assertEqual(res.status, "over-budget")
        self.assertIn("frame CPU p95 20 ms > 16.7 ms", res.detail)

    def test_a_gated_budget_fails(self):
        self.assertEqual(run(slow, "gate").status, "fail")
        self.assertEqual(run(slow, None).status, "fail")

    def test_what_did_not_work_fails_even_where_budgets_are_reported(self):
        res = run(broken_and_slow, "report")
        self.assertEqual(res.status, "fail")
        self.assertIn("no frame painted; frame CPU p95 20 ms > 16.7 ms", res.detail)

    def test_over_budget_is_counted_apart_and_does_not_fail_the_run(self):
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp)
            results = [Result("R04", "resources", "Frames", status="over-budget", detail="frame CPU p95 20 ms"),
                       Result("R01", "resources", "Idle", status="pass")]
            doc = report.write(out, {"commit_short": "abc"}, results, report.owners_from(None))
            self.assertEqual(doc["overall"], "pass")
            self.assertEqual(doc["summary"]["resources"]["over_budget"], 1)
            self.assertEqual(doc["summary"]["resources"]["fail"], 0)
            self.assertIn("**R04 Frames** (over budget, reported)", (out / "report.md").read_text())


if __name__ == "__main__":
    unittest.main()
