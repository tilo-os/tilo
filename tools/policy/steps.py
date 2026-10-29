#!/usr/bin/env python3
"""The repository policy suite's steps (tools/policy/steps.toml): what tools/policy/run.sh runs.

  steps.py list   every step as NUL-separated records for run.sh: kind (`tree` or `tests`),
                  label, command
"""
from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
import re
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = "tools/policy/steps.toml"
NAME = re.compile(r"[a-z0-9]+(?:-[a-z0-9]+)*\Z")
FIELDS = {"name", "label", "run", "tests"}
# A path of the tree in a command: a script or data file it runs or reads.
COMMAND_PATH = re.compile(r"(?<![\w./-])(?:\./)?((?:tools|scripts|os|dist|\.codex)/[\w./-]+\.(?:py|sh|json))\b")


@dataclass(frozen=True)
class Step:
    name: str
    label: str
    run: str
    tests: bool


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
                    bool(entry.get("tests")))
        if (step.tests, name) in seen:
            raise ValueError(f"{MANIFEST}: two steps are called {name}")
        seen.add((step.tests, name))
        steps.append(step)
    return steps


def scripts(step: Step) -> list[str]:
    """The files of the tree the step's command names."""
    return sorted({match[1] for match in COMMAND_PATH.finditer(step.run)})


def main(argv: list[str]) -> int:
    if argv != ["list"]:
        print(__doc__, file=sys.stderr)
        return 2
    out = sys.stdout.buffer
    for step in load():
        for field in ("tests" if step.tests else "tree", step.label, step.run):
            out.write(field.encode() + b"\0")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
