"""Unit tests for the CI check selection.

    python3 -m unittest discover -s scripts/vm-acceptance -p 'test_*.py'
"""

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from ci_select import select  # noqa: E402
import importlib.util  # noqa: E402
import tomllib  # noqa: E402

HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("acceptance_run_shards", HERE / "run.py")
run = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(run)

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

    def test_standalone_evidence_auditor_does_not_change_live_suite_results(self):
        self.assertEqual(select([S + "component-replacement.py", S + "acceptance/replacement.py"]),
                         "provenance")

    def test_the_runner_or_the_vm_door_runs_everything(self):
        self.assertEqual(select([S + "acceptance/checks/ui.py", S + "run.py"]), "")
        self.assertEqual(select([S + "acceptance/vm.py"]), "")
        self.assertEqual(select([S + "acceptance/checks/__init__.py"]), "")

    def test_source_paths_select_the_area_that_proves_them(self):
        self.assertEqual(select(["crates/dev/tilo-vmd/src/input.rs"]), "B04,W17,input")
        self.assertEqual(select(["os/hardware/profiles/lenovo-83ed/input/touchpad.json"]), "B04,W17,input")
        self.assertEqual(select(["crates/runtime/tilo-runtime/src/session_door.rs"]), "agent")
        self.assertEqual(select([S + "guest/mcp_client.py"]), "agent")
        self.assertEqual(select(["crates/system/secret-service/src/store.rs"]), "secrets")
        self.assertEqual(select(["crates/session/shell/src/portal/secret.rs"]), "secrets")
        self.assertEqual(select(["crates/session/shell/src/shell/mls_vault.rs"]), "secrets")
        both = set(select(["crates/dev/tilo-vmd/src/input.rs",
                           "crates/runtime/tilo-runtime/src/mcp.rs"]).split(","))
        self.assertEqual(both, {"B04", "W17", "input", "agent"})

    def test_unrelated_paths_in_a_whole_diff_are_ignored_and_the_runner_runs_all(self):
        self.assertEqual(select(["apps/notes/src/lib.rs", "crates/dev/tilo-vmd/src/input.rs"]), "B04,W17,input")
        self.assertEqual(select(["crates/dev/tilo-vmd/src/input.rs", "tools/ci/vm-acceptance.sh"]), "")

    def test_a_removed_check_module_runs_everything(self):
        self.assertEqual(select([S + "acceptance/checks/gone.py"]), "")


class ShardTest(unittest.TestCase):
    """t runs vm-acceptance's shards apart; together they are the whole suite."""

    def test_the_shards_are_t_tomls_and_cover_every_area_once(self):
        checks = {c["name"]: c for c in tomllib.loads((HERE.parents[1] / "t.toml").read_text())["check"]}
        self.assertEqual(checks["vm-acceptance"]["shards"], list(run.SHARDS))
        areas = [a for part in run.SHARDS.values() for a in part.split(",")]
        self.assertEqual(sorted(areas), sorted(run.core.AREAS))

    def test_a_shard_runs_its_areas_and_judges_its_own_session(self):
        window = run.select(None, run.SHARDS["window"])
        self.assertIn("W01", window)
        self.assertIn("I01", window)
        self.assertNotIn("A01", window)
        self.assertLessEqual(run.SESSION_JUDGES | run.FRAME, window)
        self.assertNotIn("R05", window)
        every = set().union(*(run.select(None, part) for part in run.SHARDS.values()))
        self.assertEqual(every, run.select(None))

    def test_a_shard_with_nothing_selected_runs_nothing_but_its_frame(self):
        self.assertEqual(run.select("W01", run.SHARDS["apps"]), run.FRAME)
        self.assertEqual(run.select("apps", run.SHARDS["session"]), run.FRAME)
        self.assertEqual(run.select("provenance", run.SHARDS["session"]), {"P01", "P02", "P03"})
        self.assertEqual(run.select("W03", run.SHARDS["window"]) - run.SESSION_JUDGES - run.FRAME,
                         {"W01", "W03"})


if __name__ == "__main__":
    unittest.main()
