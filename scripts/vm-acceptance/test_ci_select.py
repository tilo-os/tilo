"""Unit tests for the CI check selection.

    python3 -m unittest discover -s scripts/vm-acceptance -p 'test_*.py'
"""

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from ci_select import select  # noqa: E402

S = "scripts/vm-acceptance/"


class SelectTest(unittest.TestCase):
    def test_a_check_module_selects_its_checks(self):
        self.assertEqual(select([S + "acceptance/checks/search.py"]),
                         ",".join(f"S0{i}" for i in range(1, 10)))

    def test_a_module_others_import_from_selects_theirs_too(self):
        ids = set(select([S + "acceptance/checks/apps.py"]).split(","))
        # daily_apps (A08-A13) imports from apps; search does not.
        self.assertTrue({f"A{i:02}" for i in range(1, 14)} <= ids, ids)
        self.assertNotIn("S01", ids)

    def test_docs_and_unit_tests_only_prove_the_run(self):
        self.assertEqual(select([S + "README.md", S + "test_frames.py"]), "provenance")

    def test_the_runner_or_the_vm_door_runs_everything(self):
        self.assertEqual(select([S + "acceptance/checks/ui.py", S + "run.py"]), "")
        self.assertEqual(select([S + "acceptance/vm.py"]), "")
        self.assertEqual(select([S + "acceptance/checks/__init__.py"]), "")

    def test_a_removed_check_module_runs_everything(self):
        self.assertEqual(select([S + "acceptance/checks/gone.py"]), "")


if __name__ == "__main__":
    unittest.main()
