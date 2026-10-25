#!/usr/bin/env python3
"""The repository policy suite's steps (tools/policy/steps.toml): what tools/policy/run.sh runs,
and what tools/ci/t_toml.py turns into one `policy/<name>` check each.

  steps.py list   every step as NUL-separated records for run.sh: kind (`tree` or `tests`),
                  label, command (git steps wrapped in tools/ci/t-git-index.sh)
"""
from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
import re
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = "tools/policy/steps.toml"
GIT_INDEX = "tools/ci/t-git-index.sh"
FINDINGS = "tools/policy/findings.py"
NAME = re.compile(r"[a-z0-9]+(?:-[a-z0-9]+)*\Z")
FIELDS = {"name", "label", "run", "inputs", "git", "tests", "covered_by", "pre_push"}
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

    @property
    def check(self) -> str:
        """Its check in t.toml."""
        return self.covered_by or ("policy/tests/" if self.tests else "policy/") + self.name

    @property
    def command(self) -> str:
        """What runs it: under t-git-index.sh when it reads the tree through Git."""
        return f"bash {GIT_INDEX} {self.run}" if self.git else self.run

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
                    entry.get("covered_by"), bool(entry.get("pre_push")))
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


def main(argv: list[str]) -> int:
    if argv != ["list"]:
        print(__doc__, file=sys.stderr)
        return 2
    out = sys.stdout.buffer
    for step in load():
        for field in ("tests" if step.tests else "tree", step.label, step.command):
            out.write(field.encode() + b"\0")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
