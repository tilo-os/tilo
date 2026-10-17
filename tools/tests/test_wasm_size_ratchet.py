"""Tests for tools/web/wasm_size_ratchet.py."""

from __future__ import annotations

import importlib.util
import pathlib
import subprocess
import tempfile
import unittest
from unittest.mock import patch

ROOT = pathlib.Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "wasm_size_ratchet", ROOT / "tools/web/wasm_size_ratchet.py"
)
assert SPEC and SPEC.loader
ratchet = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ratchet)

BUDGET = ratchet.Budget("[bundles.desktop]\nbytes = 1000\n")
# 1 % headroom on a 64 KiB grid: a 200000-byte module gets 262144.
ROOMY = ratchet.Budget("headroom = 0.01\n[bundles.desktop]\nbytes = 262144\n")
REASON = 'reason = "the canvas gained its export dialogs"\n'


def raised(budget: ratchet.Budget, **files: str) -> ratchet.Budget:
    raises = {bundle: dict(names) for bundle, names in budget.raises.items()}
    raises.setdefault("desktop", {}).update({f"{name}.toml": text for name, text in files.items()})
    return ratchet.Budget(budget.text, raises)


def record(added: int, measured: int = 1400, run: int = 55499, reason: str = REASON) -> str:
    return f"bytes = {added}\nrun = {run}\nmeasured = {measured}\n{reason}"


class MeasureTest(unittest.TestCase):
    def bundle(self, size: int) -> pathlib.Path:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        directory = pathlib.Path(temporary.name)
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

    def test_raises_add_to_the_budget(self) -> None:
        self.assertEqual(ratchet.measure("desktop", self.bundle(1300), raised(BUDGET, a=record(400)))[0], 0)

    def test_shrinking_asks_for_a_lower_budget(self) -> None:
        status, message = ratchet.measure("desktop", self.bundle(100000), ROOMY)
        self.assertEqual(status, 0)
        self.assertIn("bytes = 131072, without its raise files", message)

    def test_a_budget_is_the_measurement_plus_headroom_on_a_64_kib_grid(self) -> None:
        self.assertEqual(ratchet.budget_for(200000, 0.01), 262144)
        self.assertEqual(ratchet.budget_for(259548, 0.01), 262144)
        self.assertEqual(ratchet.budget_for(259549, 0.01), 327680)
        self.assertEqual(ratchet.budget_for(65536, 0), 65536)

    def test_growth_names_the_raise_to_add(self) -> None:
        self.assertEqual(ratchet.measure("desktop", self.bundle(200113), ROOMY)[0], 0)
        status, message = ratchet.measure("desktop", self.bundle(262145), ROOMY)
        self.assertEqual(status, 1)
        self.assertIn("raises/desktop/<change>.toml with bytes = 65536, measured = 262145", message)

    def test_a_shrink_inside_the_rounding_asks_for_nothing(self) -> None:
        status, message = ratchet.measure("desktop", self.bundle(199000), ROOMY)
        self.assertEqual(status, 0)
        self.assertNotIn("lower it", message)

    def test_a_bundle_without_a_budget_fails_with_the_entry_to_add(self) -> None:
        status, message = ratchet.measure("starter", self.bundle(10), BUDGET)
        self.assertEqual(status, 1)
        self.assertIn("[bundles.starter] bytes = 65536", message)

    def test_the_checked_in_budget_parses(self) -> None:
        budget = ratchet.load()
        self.assertIn("desktop", budget.totals())
        for bundle in budget.raises:
            for name, entry in budget.raise_records(bundle).items():
                self.assertEqual(ratchet.raise_problems(bundle, name, entry, budget.headroom), [])


class BudgetRatchetTest(unittest.TestCase):
    def test_budgets_may_only_decrease(self) -> None:
        self.assertEqual(ratchet.increases(BUDGET, ratchet.Budget("[bundles.desktop]\nbytes = 900\n")), [])
        self.assertTrue(ratchet.increases(BUDGET, ratchet.Budget("[bundles.desktop]\nbytes = 1001\n")))
        self.assertTrue(ratchet.increases(BUDGET, ratchet.Budget("")))

    def test_a_new_raise_file_may_raise_a_budget_by_its_bytes(self) -> None:
        after = raised(BUDGET, canvas=record(400))
        self.assertEqual(ratchet.increases(BUDGET, after), [])
        # An existing raise file allows nothing more.
        grown = ratchet.Budget("[bundles.desktop]\nbytes = 1100\n", after.raises)
        self.assertTrue(ratchet.increases(after, grown))
        # Nor does a raise smaller than the growth.
        self.assertTrue(ratchet.increases(BUDGET, ratchet.Budget("[bundles.desktop]\nbytes = 1500\n",
                                                                 after.raises)))

    def test_concurrent_raises_add_up(self) -> None:
        one = raised(BUDGET, canvas=record(400))
        both = raised(one, notes=record(300))
        self.assertEqual(both.totals()["desktop"], 1700)
        self.assertEqual(ratchet.increases(one, both), [])

    def test_folding_raises_into_a_lower_base_is_a_decrease(self) -> None:
        before = raised(BUDGET, canvas=record(400))
        self.assertEqual(ratchet.increases(before, ratchet.Budget("[bundles.desktop]\nbytes = 1300\n")), [])

    def test_a_raise_needs_a_run_a_measurement_and_a_reason(self) -> None:
        for weak in ("bytes = 400\nmeasured = 1400\n" + REASON,
                     "bytes = 400\nrun = 55499\n" + REASON,
                     record(400, reason='reason = "bigger"\n'),
                     record(0),
                     record(400000, measured=1400)):
            with self.subTest(weak=weak):
                self.assertTrue(ratchet.increases(BUDGET, raised(BUDGET, weak=weak)))

    def test_new_bundles_may_be_added(self) -> None:
        after = ratchet.Budget(BUDGET.text + "[bundles.starter]\nbytes = 5\n")
        self.assertEqual(ratchet.increases(BUDGET, after), [])


class RevisionTest(unittest.TestCase):
    def test_unobservable_base_is_not_treated_as_a_revision_without_budgets(self):
        with tempfile.TemporaryDirectory() as directory:
            repo = pathlib.Path(directory)
            def git(*args):
                return subprocess.run(["git", "-C", str(repo), *args],
                                      check=True, capture_output=True, text=True)
            git("init", "-q")
            git("-c", "user.name=t", "-c", "user.email=t@invalid",
                "commit", "--allow-empty", "-qm", "empty baseline")
            self.assertIsNone(ratchet.load_at("HEAD", repo))
            with self.assertRaisesRegex(RuntimeError, "cannot resolve"):
                ratchet.load_at("absent-revision", repo)
            path = repo / ratchet.DIRECTORY / "budget.toml"
            path.parent.mkdir(parents=True)
            path.write_text(BUDGET.text)
            git("add", ".")
            git("-c", "user.name=t", "-c", "user.email=t@invalid", "commit", "-qm", "budget")
            run = subprocess.run
            for failed in ("ls-tree", "show"):
                def fail_read(argv, *args, **kwargs):
                    if failed in argv:
                        return subprocess.CompletedProcess(argv, 128, "", "object read failed")
                    return run(argv, *args, **kwargs)
                with self.subTest(failed=failed), patch.object(ratchet.subprocess, "run", fail_read):
                    with self.assertRaisesRegex(RuntimeError, "object read failed"):
                        ratchet.load_at("HEAD", repo)
            self.assertEqual(ratchet.load_at("HEAD", repo).totals(), BUDGET.totals())

    def test_reads_the_layout_of_a_revision(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        repo = pathlib.Path(temporary.name)
        git = lambda *args: subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True)
        git("init", "-q")
        legacy = repo / ratchet.LEGACY
        legacy.parent.mkdir(parents=True)
        legacy.write_text("[bundles.desktop]\nbytes = 1000\n")
        git("add", "-A")
        git("-c", "user.name=t", "-c", "user.email=t@invalid", "commit", "-qm", "legacy")
        self.assertEqual(ratchet.load_at("HEAD", repo).totals(), {"desktop": 1000})
        legacy.unlink()
        raises = repo / ratchet.DIRECTORY / "raises/desktop"
        raises.mkdir(parents=True)
        (repo / ratchet.DIRECTORY / "budget.toml").write_text("[bundles.desktop]\nbytes = 900\n")
        (raises / "canvas.toml").write_text(record(400))
        git("add", "-A")
        git("-c", "user.name=t", "-c", "user.email=t@invalid", "commit", "-qm", "directory")
        self.assertEqual(ratchet.load_at("HEAD", repo).totals(), {"desktop": 1300})
        self.assertEqual(ratchet.load(repo).totals(), {"desktop": 1300})


if __name__ == "__main__":
    unittest.main()
