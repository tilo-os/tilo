"""A run narrowed with `t test <check> --test` runs only the tests it names.

t hands a check that declares `tests = "env"` (t.toml) the names in T_TESTS, one per line. This
holds tools/ci/test_selection.py to t's own meaning of a name (a test, or a module path for every
test below it: src/drivers/cargo/exec/filters.rs), and tools/ci/nextest.sh to intersecting each
nextest command it wraps with the selection, which the flows' checks depend on: a flow's check
runs a whole flow's tests otherwise.
"""
from __future__ import annotations

import importlib.util
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
T_NEXTEST = REPO / "tools" / "ci" / "nextest.sh"


def load(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


selection = load("test_selection", REPO / "tools" / "ci" / "test_selection.py")


class FilterTests(unittest.TestCase):
    """The filterset a selection is, as nextest reads it."""

    def test_a_full_name_selects_that_test_and_the_module_it_names(self) -> None:
        # `parser` names the test called `parser` in any module and the module `parser` and every
        # test below it, never `parser_extra`: a name is a path segment.
        self.assertEqual(selection.name_filter("parser"), "test(/(?:^|::)parser(?:$|::)/)")

    def test_a_module_path_selects_everything_below_it(self) -> None:
        self.assertEqual(
            selection.name_filter("workspace_actions::flow_customer_workspaces::"),
            "test(/(?:^|::)workspace_actions::flow_customer_workspaces::/)",
        )

    def test_a_name_outside_the_path_alphabet_is_escaped(self) -> None:
        # A test name may carry a case table's brackets or a space: as a regex they would mean
        # something else or nothing.
        self.assertEqual(selection.name_filter("case [1]"), "test(/(?:^|::)case\\ \\[1\\](?:$|::)/)")

    def test_several_names_are_one_alternative(self) -> None:
        self.assertEqual(
            selection.expression(["a::b", "", "  ", "c::"]),
            "test(/(?:^|::)a::b(?:$|::)/) | test(/(?:^|::)c::/)",
        )
        self.assertEqual(selection.expression([]), "")


class WrapperTests(unittest.TestCase):
    """tools/ci/nextest.sh runs the selection's tests alone, through the production wrapper."""

    def run_wrapper(self, tests: str, command: tuple[str, ...]):
        """The wrapper's result and the cargo invocations it made, in order."""
        with tempfile.TemporaryDirectory() as work:
            tree, programs = Path(work) / "tree", Path(work) / "bin"
            (tree / "tools" / "ci").mkdir(parents=True)
            (tree / ".config").mkdir()
            programs.mkdir()
            for file in ("nextest.sh", "handed.sh", "test_selection.py", "nextest_filters.py"):
                shutil.copy(REPO / "tools" / "ci" / file, tree / "tools" / "ci" / file)
            shutil.copy(REPO / ".config" / "nextest-filters.toml", tree / ".config" / "nextest-filters.toml")
            argv = Path(work) / "argv"
            (programs / "cargo").write_text('#!/bin/sh\nprintf "%s\\n" "$*" >>"$GIVEN"\n')
            (programs / "cargo").chmod(0o755)
            env = {"PATH": f"{programs}{os.pathsep}{os.environ.get('PATH', '/usr/bin:/bin')}",
                   "GIVEN": str(argv), "T_TESTS": tests}
            result = subprocess.run(["bash", str(tree / "tools" / "ci" / "nextest.sh"), *command],
                                    env=env, capture_output=True, text=True, check=False)
            ran = argv.read_text().splitlines() if argv.exists() else []
            return result, ran

    RUN = ("cargo", "nextest", "run", "--locked", "--profile", "ci", "--no-tests=pass",
           "-p", "tilo-agent-session", "--test", "it",
           "-E", "test(workspace_actions::flow_customer_workspaces::)")

    def test_the_run_is_narrowed_to_the_tests_named(self) -> None:
        result, ran = self.run_wrapper(
            "workspace_actions::flow_customer_workspaces::one", self.RUN)
        self.assertEqual(result.returncode, 0, result.stderr)
        # A run handed none of the programs its package's tests start builds them first.
        self.assertTrue(ran[-1].startswith("nextest run"), ran)
        self.assertIn(
            "(test(workspace_actions::flow_customer_workspaces::)) & "
            "(test(/(?:^|::)workspace_actions::flow_customer_workspaces::one(?:$|::)/))",
            ran[-1],
        )

    def test_a_command_with_no_test_named_gets_the_selection_as_its_filter(self) -> None:
        # A command that names no filter of its own is narrowed to the selection alone, beside
        # the shared filters every such run leaves out: it still runs, and nextest passes it with
        # none of its tests met.
        command = ("cargo", "nextest", "run", "--locked", "--profile", "ci", "--no-tests=pass",
                   "-p", "tilo-agent-session", "--test", "it")
        result, ran = self.run_wrapper("x::y", command)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("-E (not (", ran[-1], "beside the shared filters")
        self.assertIn("& (test(/(?:^|::)x::y(?:$|::)/))", ran[-1])

    def test_a_run_with_no_selection_is_its_command_unchanged(self) -> None:
        result, ran = self.run_wrapper("", self.RUN)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("-E test(workspace_actions::flow_customer_workspaces::)", ran[-1])
        self.assertNotIn("(?:^|::)", ran[-1])


if __name__ == "__main__":
    unittest.main()
