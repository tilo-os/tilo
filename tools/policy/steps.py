#!/usr/bin/env python3
"""The repository policy suite's steps (tools/policy/steps.toml): what tools/policy/run.sh runs.

  steps.py list [--node | --no-node]
                  every step as NUL-separated records for run.sh: kind (`tree` or `tests`),
                  label, command. With --no-node, the steps without their `node_tests`; with
                  --node, only those tests, one step per step that names some: run.sh runs the
                  suite in two halves where no machine has both Node.js and the Git checkout
                  (tools/ci/local_affected.py runs the --node half on the builder)
"""
from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
import re
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = "tools/policy/steps.toml"
SPLIT_RUNNER = "tools/policy/run_unit_tests.py"
NAME = re.compile(r"[a-z0-9]+(?:-[a-z0-9]+)*\Z")
FIELDS = {"name", "label", "run", "tests", "node_tests"}
# A path of the tree in a command: a script or data file it runs or reads.
COMMAND_PATH = re.compile(r"(?<![\w./-])(?:\./)?((?:tools|scripts|os|dist|\.codex)/[\w./-]+\.(?:py|sh|json))\b")


@dataclass(frozen=True)
class Step:
    name: str
    label: str
    run: str
    tests: bool
    # Test files of a tests step that run Node.js scripts. They read no Git checkout, so where
    # Node.js and the checkout are on different machines they run apart from the rest
    # (`steps.py list --node`).
    node_tests: tuple[str, ...] = ()

    @property
    def without_node(self) -> str:
        """Its command without its `node_tests` (run_unit_tests.py --except)."""
        return " ".join([self.run, *(f"--except {test}" for test in self.node_tests)])

    @property
    def node_command(self) -> str:
        """What runs just its `node_tests`."""
        return " ".join([f"python3 {SPLIT_RUNNER}", *self.node_tests])


def load(root: Path = ROOT) -> list[Step]:
    data = tomllib.loads((root / MANIFEST).read_text(encoding="utf-8"))
    steps, seen = [], set()
    for entry in data.get("step", []):
        unknown = set(entry) - FIELDS
        if unknown:
            raise ValueError(f"{MANIFEST}: step {entry.get('name')!r} has unknown fields {sorted(unknown)}")
        name = entry.get("name", "")
        if not NAME.match(name):
            raise ValueError(f"{MANIFEST}: a step's name is lowercase words joined by '-', not {name!r}")
        step = Step(name, entry["label"], " ".join(entry["run"].replace("\\\n", " ").split()),
                    bool(entry.get("tests")), tuple(entry.get("node_tests", ())))
        if step.node_tests:
            if not (step.tests and step.run.startswith(f"python3 {SPLIT_RUNNER} ")):
                raise ValueError(f"{MANIFEST}: step {name} names node_tests, which only a tests step run by "
                                 f"{SPLIT_RUNNER} may")
            missing = [test for test in step.node_tests if not (root / test).is_file()]
            if missing:
                raise ValueError(f"{MANIFEST}: step {name} names node_tests that don't exist: {', '.join(missing)}")
        if (step.tests, name) in seen:
            raise ValueError(f"{MANIFEST}: two steps are called {name}")
        seen.add((step.tests, name))
        steps.append(step)
    return steps


def scripts(step: Step) -> list[str]:
    """The files of the tree the step's command names."""
    return sorted({match[1] for match in COMMAND_PATH.finditer(step.run)})


def listed(steps: list[Step], node: bool | None) -> list[tuple[str, str, str]]:
    """run.sh's records: every step (`node` None), those without their node tests (False), or
    just the node tests (True)."""
    if node is None:
        return [("tests" if step.tests else "tree", step.label, step.run) for step in steps]
    if node:
        return [("tests", f"{step.label} that run Node.js scripts", step.node_command)
                for step in steps if step.node_tests]
    return [("tests" if step.tests else "tree", step.label, step.without_node) for step in steps]


def main(argv: list[str]) -> int:
    halves = {(): None, ("--node",): True, ("--no-node",): False}
    if argv[:1] != ["list"] or tuple(argv[1:]) not in halves:
        print(__doc__, file=sys.stderr)
        return 2
    out = sys.stdout.buffer
    for record in listed(load(), halves[tuple(argv[1:])]):
        for field in record:
            out.write(field.encode() + b"\0")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
