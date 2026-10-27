import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import tomllib
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


def lock(deps: dict[str, list[str]]) -> str:
    """A Cargo.lock of packages at 0.1.0 with `deps`: package -> [dependency]."""
    return "version = 4\n" + "".join(
        f'\n[[package]]\nname = "{name}"\nversion = "0.1.0"\n'
        + (f"dependencies = [{', '.join(json.dumps(d) for d in ds)}]\n" if ds else "")
        for name, ds in deps.items()
    )


def members(deps: dict[str, list[tuple[str, str | None, bool]]]) -> dict:
    """``cargo metadata --no-deps`` of members with `deps`: member ->
    [(dependency, kind, whether it is a path dependency)]."""
    return {
        "packages": [
            {
                "name": name,
                "version": "0.1.0",
                "dependencies": [
                    {"name": dep, "kind": kind, "path": f"/src/{dep}" if path else None}
                    for dep, kind, path in ds
                ],
            }
            for name, ds in deps.items()
        ]
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

    def test_an_acyclic_workspace_passes(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "Cargo.lock").write_text(lock({"app": ["base"], "base": []}))
            stdout = json.dumps(members({"app": [("base", None, True)], "base": []}))
            self.assertEqual(check.audit(root, run=ran(0, stdout=stdout)), ([], []))

    def test_a_cycle_through_the_lock_fails_with_its_cycle(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "Cargo.lock").write_text(lock({"app": ["base"], "base": ["app"]}))
            stdout = json.dumps(members({"app": [("base", None, True)], "base": [("app", None, True)]}))
            found, _ = check.audit(root, run=ran(0, stdout=stdout))
            self.assertEqual(len(found), 1)
            self.assertTrue(found[0].startswith("Cargo.lock: "))
            self.assertIn("app", found[0])

    def test_other_cargo_failures_are_reported(self):
        found, notes = check.audit(run=ran(101, stderr="error: failed to parse manifest"))
        self.assertEqual(found, [])
        self.assertIn("could not be checked", notes[0])

    def test_a_members_dev_dependency_in_the_lock_closes_no_loop(self):
        # The lock lists a member's dev-dependencies among its dependencies.
        graph = check.graph(
            members({"low": [("high", "dev", True)], "high": [("low", None, True)]}),
            tomllib.loads(lock({"low": ["high"], "high": ["low"]})),
        )
        self.assertIsNone(check.find_cycle(graph))

    def test_a_new_path_dependency_counts_before_the_lock_has_it(self):
        graph = check.graph(
            members({"low": [("high", None, True)], "high": [("low", None, True)]}),
            tomllib.loads(lock({"low": [], "high": ["low"]})),
        )
        self.assertEqual(set(check.find_cycle(graph) or []), {"low", "high"})

    def test_a_cycle_through_a_registry_crate_is_found(self):
        # A vendored member patched in for a registry crate's dependency: the
        # registry crate's edges come from the lock alone.
        graph = check.graph(
            members({"app": [("serde-ext", None, False)], "vendored": [("app", None, True)]}),
            tomllib.loads(lock({"app": ["serde-ext"], "serde-ext": ["vendored"], "vendored": ["app"]})),
        )
        self.assertEqual(set(check.find_cycle(graph) or []), {"app", "serde-ext", "vendored"})

    def test_it_needs_no_registry_cache(self):
        with tempfile.TemporaryDirectory() as home:
            env = {**os.environ, "CARGO_HOME": home}
            result = subprocess.run(
                [sys.executable, str(SCRIPT)], capture_output=True, text=True, env=env, check=False
            )
            self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
