"""Tests for tools/web/wasm_size_ratchet.py."""

from __future__ import annotations

import importlib.util
import pathlib
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "wasm_size_ratchet", ROOT / "tools/web/wasm_size_ratchet.py"
)
assert SPEC and SPEC.loader
ratchet = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ratchet)

BUDGET = "[bundles.desktop]\nbytes = 1000\n"


class MeasureTest(unittest.TestCase):
    def bundle(self, size: int) -> pathlib.Path:
        directory = pathlib.Path(tempfile.mkdtemp())
        module = directory / ratchet.MODULE
        module.parent.mkdir(parents=True)
        module.write_bytes(b"\0" * size)
        return directory

    def test_growth_past_the_budget_fails(self) -> None:
        status, message = ratchet.measure("desktop", self.bundle(1001), BUDGET)
        self.assertEqual(status, 1)
        self.assertIn("over its budget", message)

    def test_within_the_budget_passes(self) -> None:
        self.assertEqual(ratchet.measure("desktop", self.bundle(995), BUDGET)[0], 0)

    def test_shrinking_asks_for_a_lower_budget(self) -> None:
        status, message = ratchet.measure("desktop", self.bundle(900), BUDGET)
        self.assertEqual(status, 0)
        self.assertIn("lower it to 900", message)

    def test_a_bundle_without_a_budget_fails_with_the_entry_to_add(self) -> None:
        status, message = ratchet.measure("starter", self.bundle(10), BUDGET)
        self.assertEqual(status, 1)
        self.assertIn("[bundles.starter] bytes = 10", message)

    def test_the_checked_in_budget_parses(self) -> None:
        self.assertIn("desktop", ratchet.budgets(ratchet.BUDGET.read_text()))


class BudgetRatchetTest(unittest.TestCase):
    def test_budgets_may_only_decrease(self) -> None:
        self.assertEqual(ratchet.increases(BUDGET, "[bundles.desktop]\nbytes = 900\n"), [])
        self.assertTrue(ratchet.increases(BUDGET, "[bundles.desktop]\nbytes = 1001\n"))
        self.assertTrue(ratchet.increases(BUDGET, ""))

    def test_a_recorded_rebaseline_may_raise_a_budget_once(self) -> None:
        record = '[bundles.desktop.rebaseline]\nrun = 55499\nreason = "the lane first reached its size check"\n'
        raised = "[bundles.desktop]\nbytes = 1500\n" + record
        self.assertEqual(ratchet.increases(BUDGET, raised), [])
        # The same record cannot raise it again.
        self.assertTrue(ratchet.increases(raised, "[bundles.desktop]\nbytes = 1600\n" + record))
        # Nor can a record without a run or a real reason.
        for weak in ('[bundles.desktop.rebaseline]\nreason = "the lane first reached its size check"\n',
                     '[bundles.desktop.rebaseline]\nrun = 55499\nreason = "bigger"\n'):
            self.assertTrue(ratchet.increases(BUDGET, "[bundles.desktop]\nbytes = 1500\n" + weak))

    def test_new_bundles_may_be_added(self) -> None:
        after = BUDGET + "[bundles.starter]\nbytes = 5\n"
        self.assertEqual(ratchet.increases(BUDGET, after), [])


if __name__ == "__main__":
    unittest.main()
