#!/usr/bin/env python3
"""The repository policy suite's steps (tools/policy/steps.toml): what tools/policy/run.sh runs,
and what tools/ci/t_toml.py turns into one `policy/<name>` check each.

  steps.py list [--node | --no-node]
                  every step as NUL-separated records for run.sh: kind (`tree` or `tests`),
                  label, command (git steps wrapped in tools/ci/t-git-index.sh). With
                  --no-node, the steps without their `node_tests`; with --node, only those
                  tests, one step per step that names some: run.sh runs the suite in two
                  halves where no machine has both Node.js and the Git checkout
                  (tools/ci/local_affected.py runs the --node half on the builder)
"""
from __future__ import annotations

from dataclasses import dataclass, replace
from pathlib import Path, PurePath
import re
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = "tools/policy/steps.toml"
GIT_INDEX = "tools/ci/t-git-index.sh"
FINDINGS = "tools/policy/findings.py"
SPLIT_RUNNER = "tools/policy/run_unit_tests.py"
NAME = re.compile(r"[a-z0-9]+(?:-[a-z0-9]+)*\Z")
FIELDS = {"name", "label", "run", "inputs", "git", "tests", "history", "covered_by", "pre_push", "split", "whole_tree_tests",
          "node_tests"}
# What a step a push runs may not be: it lists the tree through Git (t indexes all of it first),
# reads the whole tree, or builds. Those wait for main, which runs every default check.
WHOLE_TREE = frozenset({"**"})
BUILD = re.compile(r"(?:^|[\s;&|(])cargo\s+(?:build|test|run|clippy|install)\b")
# Directories whose modules a step's Python imports by bare name (each script puts its own
# directory, or one of these, on sys.path).
MODULE_DIRS = ("tools/policy", "tools/ci", "tools/docs")
IMPORT = re.compile(r"^\s*(?:from\s+([A-Za-z_][\w.]*)\s+import|import\s+([A-Za-z_][\w.]*))", re.MULTILINE)
# A path of the tree in a command: a script or data file it runs or reads.
COMMAND_PATH = re.compile(r"(?<![\w./-])(?:\./)?((?:tools|scripts|os|dist|\.codex)/[\w./-]+\.(?:py|sh|json))\b")


@dataclass(frozen=True)
class Step:
    name: str
    label: str
    run: str
    inputs: tuple[str, ...]
    git: bool
    tests: bool
    covered_by: str | None
    pre_push: bool = False
    history: bool = False
    # It only lists the tracked paths through Git (`git = "paths"`), never reads blobs from the
    # index, so t-git-index.sh indexes the paths without hashing the files.
    git_paths: bool = False
    # The suite's tests, one check per test file (`split = true`): see `expanded`.
    split: bool = False
    # Test files that run in the whole tree, with t's .git, whatever unit_test_inputs.py finds in
    # them: those that read the commit's history, and those that check the tree as a whole.
    whole_tree_tests: tuple[str, ...] = ()
    # It runs in a copy of the tree holding only its inputs (run_unit_tests.py --pruned).
    pruned: bool = False
    # Test files of a tests step that run Node.js scripts. They read no Git checkout, so where
    # Node.js and the checkout are on different machines they run apart from the rest
    # (`steps.py list --node`).
    node_tests: tuple[str, ...] = ()

    @property
    def without_node(self) -> str:
        """Its command without its `node_tests` (run_unit_tests.py --except)."""
        return " ".join([self.command, *(f"--except {test}" for test in self.node_tests)])

    @property
    def node_command(self) -> str:
        """What runs just its `node_tests`."""
        return " ".join([f"python3 {SPLIT_RUNNER}", *self.node_tests])

    @property
    def check(self) -> str:
        """Its check in t.toml."""
        return self.covered_by or ("policy/tests/" if self.tests else "policy/") + self.name

    @property
    def command(self) -> str:
        """What runs it: under t-git-index.sh when it reads the tree through Git."""
        if not self.git:
            return self.run
        return f"bash {GIT_INDEX} --paths {self.run}" if self.git_paths else f"bash {GIT_INDEX} {self.run}"

    @property
    def t_command(self) -> str:
        """What t runs: a check of the tree reports each of its findings for t to compare with
        main's (findings.py); the suite's tests say which test failed themselves."""
        return self.command if self.tests else f"python3 {FINDINGS} {self.command}"


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
                    tuple(entry.get("inputs", ())), bool(entry.get("git")), bool(entry.get("tests")),
                    entry.get("covered_by"), bool(entry.get("pre_push")), bool(entry.get("history")),
                    entry.get("git") == "paths", bool(entry.get("split")), tuple(entry.get("whole_tree_tests", ())),
                    node_tests=tuple(entry.get("node_tests", ())))
        if step.node_tests:
            if not (step.tests and step.run.startswith(f"python3 {SPLIT_RUNNER} ")):
                raise ValueError(f"{MANIFEST}: step {name} names node_tests, which only a tests step run by "
                                 f"{SPLIT_RUNNER} may")
            missing = [test for test in step.node_tests if not (root / test).is_file()]
            if missing:
                raise ValueError(f"{MANIFEST}: step {name} names node_tests that don't exist: {', '.join(missing)}")
            whole = sorted(set(step.node_tests) & set(step.whole_tree_tests))
            if whole:
                raise ValueError(f"{MANIFEST}: step {name}: {', '.join(whole)} reads the whole tree, so it cannot "
                                 "run apart from the Git checkout as a node test")
        if step.split and not (step.tests and step.history):
            raise ValueError(f"{MANIFEST}: step {name} says split = true, which only the suite's tests with "
                             "history = true may (the tests that read the whole tree run as one such check)")
        if entry.get("git") not in (None, True, False, "paths"):
            raise ValueError(f"{MANIFEST}: step {name} says git = {entry.get('git')!r}: true or \"paths\"")
        if step.history and not step.tests:
            raise ValueError(f"{MANIFEST}: step {name} says history = true, which only the suite's tests may")
        if (step.tests, name) in seen:
            raise ValueError(f"{MANIFEST}: two steps are called {name}")
        if step.covered_by is None and not step.inputs:
            raise ValueError(f"{MANIFEST}: step {name} names no inputs")
        if step.covered_by is not None and (step.inputs or step.tests):
            raise ValueError(f"{MANIFEST}: step {name} is covered by {step.covered_by}, whose declaration says "
                             "what it reads")
        if step.pre_push and not step.tests:
            why = ("it lists the tree through Git" if step.git else
                   "it reads the whole tree" if WHOLE_TREE & set(step.inputs) else
                   "it builds" if BUILD.search(step.run) else "")
            if why:
                raise ValueError(f"{MANIFEST}: step {name} says pre_push = true, but {why}: a push runs only "
                                 "the steps that build nothing and read no Git index of the tree")
        seen.add((step.tests, name))
        steps.append(step)
    return steps


def split_checks(step: Step, root: Path = ROOT) -> list[Step]:
    """A `split` step as t runs it: each test file that reads only some of the tree is a check of
    its own, `<check>/<name>`, keyed by what it reads (tools/policy/unit_test_inputs.py) and run in
    a copy of the tree holding just that, so a read of anything else fails it; the files that read
    the whole tree, or the commit's history, stay together in the step's own check."""
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    import unit_test_inputs

    directories = [Path(word) for word in step.run.split()[2:]]
    tree = unit_test_inputs.Tree(root)
    tests = sorted(path for path in tree.files
                   if any(PurePath(path).parent == directory for directory in directories)
                   and PurePath(path).match("test_*.py"))
    missing = sorted(set(step.whole_tree_tests) - set(tests))
    if missing:
        raise ValueError(f"{MANIFEST}: step {step.name} names whole-tree tests that don't exist: {', '.join(missing)}")
    whole, own = [], []
    for test in tests:
        found, _ = (None, "") if test in step.whole_tree_tests else unit_test_inputs.inputs(test, tree)
        if found is None:
            whole.append(test)
        else:
            name = PurePath(test).stem.removeprefix("test_").replace("_", "-")
            own.append(replace(step, name=f"{step.name}/{name}", run=f"python3 {SPLIT_RUNNER} --pruned {test}",
                               inputs=tuple(found), history=False, pre_push=step.pre_push, split=False,
                               whole_tree_tests=(), pruned=True))
    rest = replace(step, run=f"python3 {SPLIT_RUNNER} {' '.join(whole)}", split=False, whole_tree_tests=())
    return [rest, *own]


def expanded(root: Path = ROOT) -> list[Step]:
    """Every step as t runs it: a `split` step as its checks (`split_checks`)."""
    return [check for step in load(root) for check in (split_checks(step, root) if step.split else [step])]


def imports(path: str, root: Path = ROOT, found: set[str] | None = None) -> set[str]:
    """`path` and the repository modules it imports, transitively."""
    found = set() if found is None else found
    if path in found or not (root / path).is_file():
        return found
    found.add(path)
    if not path.endswith(".py"):
        return found
    text = (root / path).read_text(encoding="utf-8", errors="replace")
    for match in IMPORT.finditer(text):
        dotted = (match[1] or match[2]).split(".")
        for directory in (str(Path(path).parent), *MODULE_DIRS):
            candidate = f"{directory}/{dotted[-1]}.py"
            if (root / candidate).is_file():
                imports(candidate, root, found)
                break
        else:
            # `from policy.rust_source import ...` with tools/ on the path.
            candidate = "tools/" + "/".join(dotted) + ".py"
            if (root / candidate).is_file():
                imports(candidate, root, found)
    return found


def own_files(step: Step, root: Path = ROOT) -> list[str]:
    """The files of the step's own command: the scripts it names and the modules they import."""
    files: set[str] = set()
    for match in COMMAND_PATH.finditer(step.run):
        imports(match[1], root, files)
    if step.git:
        files.add(GIT_INDEX)
    if not step.tests:
        files.add(FINDINGS)
    return sorted(files)


def listed(steps: list[Step], node: bool | None) -> list[tuple[str, str, str]]:
    """run.sh's records: every step (`node` None), those without their node tests (False), or
    just the node tests (True)."""
    if node is None:
        return [("tests" if step.tests else "tree", step.label, step.command) for step in steps]
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
