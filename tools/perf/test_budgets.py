"""Tests of the KPI budgets (tools/perf/kpi-budgets.toml, read by compare.py)."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import compare  # noqa: E402


class Budgets(unittest.TestCase):
    def test_the_budgets_load(self):
        budgets = compare.load_budgets(compare.BUDGETS)
        for kpi, entry in budgets.kpis.items():
            with self.subTest(kpi=kpi):
                ceilings = [name for name in compare.CEILINGS if name in entry]
                self.assertTrue(ceilings or entry.get("baseline_only"),
                                "a budget names a ceiling, or says baseline_only")
                self.assertFalse(ceilings and entry.get("baseline_only"),
                                 "a baseline_only series has no ceiling to miss")


if __name__ == "__main__":
    unittest.main()
