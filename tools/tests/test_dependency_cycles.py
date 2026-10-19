import importlib.util
import json
import subprocess
import sys
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "policy/check_dependency_cycles.py"
SPEC = importlib.util.spec_from_file_location("check_dependency_cycles", SCRIPT)
check = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = check
SPEC.loader.exec_module(check)


def metadata(edges: dict[str, list[tuple[str, str]]]) -> dict:
    """Metadata whose resolve has `edges`: package -> [(dependency, kind)]."""
    return {
        "packages": [{"id": f"{name} 0.1.0", "name": name} for name in edges],
        "resolve": {
            "nodes": [
                {
                    "id": f"{name} 0.1.0",
                    "deps": [
                        {"pkg": f"{dep} 0.1.0", "dep_kinds": [{"kind": kind}]}
                        for dep, kind in deps
                    ],
                }
                for name, deps in edges.items()
            ]
        },
    }


def ran(returncode: int, stdout: str = "", stderr: str = ""):
    return lambda _root: subprocess.CompletedProcess([], returncode, stdout, stderr)


class DependencyCycleTests(unittest.TestCase):
    def test_a_normal_cycle_is_named(self):
        graph = metadata(
            {
                "tilo-resource": [("tilo-content", None)],
                "tilo-content": [("runtime-fs", None)],
                "runtime-fs": [("collab-types", "build")],
                "collab-types": [("tilo-resource", None)],
            }
        )
        cycle = check.find_cycle(graph)
        self.assertIsNotNone(cycle)
        self.assertEqual(cycle[0], cycle[-1])
        self.assertEqual(
            set(cycle), {"tilo-resource", "tilo-content", "runtime-fs", "collab-types"}
        )

    def test_dev_dependencies_may_close_a_loop(self):
        graph = metadata(
            {
                "tilo-resource": [("tilo-content", None)],
                "tilo-content": [("tilo-resource", "dev")],
            }
        )
        self.assertIsNone(check.find_cycle(graph))

    def test_a_shared_dependency_is_no_cycle(self):
        graph = metadata(
            {
                "app": [("left", None), ("right", None)],
                "left": [("base", None)],
                "right": [("base", None)],
                "base": [],
            }
        )
        self.assertIsNone(check.find_cycle(graph))

    def test_cargos_cycle_error_fails_with_its_cycle(self):
        stderr = (
            "error: cyclic package dependency: package `tilo-resource v0.1.0` "
            "depends on itself. Cycle:\n"
        )
        failure = check.audit(run=ran(101, stderr=stderr))
        self.assertTrue(failure)
        self.assertIn("cycle", failure[0])
        self.assertTrue(any("tilo-resource" in line for line in failure))

    def test_an_acyclic_workspace_passes(self):
        stdout = json.dumps(metadata({"app": [("base", None)], "base": []}))
        self.assertEqual(check.audit(run=ran(0, stdout=stdout)), [])

    def test_other_cargo_failures_are_reported(self):
        failure = check.audit(run=ran(101, stderr="error: the lock file needs to be updated"))
        self.assertTrue(failure)
        self.assertIn("could not be checked", failure[0])


if __name__ == "__main__":
    unittest.main()
