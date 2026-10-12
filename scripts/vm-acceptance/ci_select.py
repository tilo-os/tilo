#!/usr/bin/env python3
"""Which checks a change to the suite touches, as an `--only` value for run.py.

    scripts/vm-acceptance/ci_select.py <changed path>...

Prints the check ids defined in the changed check modules (and in the modules that
import from them), `provenance` when only documentation or unit tests changed, or
nothing when anything else changed (the runner, the VM door, the report): then the
whole suite runs. P01/P02 come with any selection (run.py adds them).
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
PREFIX = "scripts/vm-acceptance/"
CHECKS = "acceptance/checks/"
# Not part of a run: what they change cannot change a result.
INERT = re.compile(r"^(README\.md|test_[a-z_]+\.py|prune_disks\.py|build-disk\.sh|"
                   r"component-replacement\.py|acceptance/replacement\.py)$")


def modules_importing(modules: set[str], checks_dir: Path) -> set[str]:
    """`modules` plus every check module that imports from one of them, transitively."""
    imports = {p.stem: set(re.findall(r"^from \.(\w+) import", p.read_text(), re.M))
               for p in checks_dir.glob("*.py")}
    out = set(modules)
    while True:
        more = {m for m, deps in imports.items() if deps & out} - out
        if not more:
            return out
        out |= more


def select(changed: list[str], suite: Path = HERE) -> str:
    sys.path.insert(0, str(suite))
    from acceptance import core  # noqa: PLC0415
    import importlib  # noqa: PLC0415
    for module in sorted((suite / CHECKS).glob("*.py")):
        if module.stem != "__init__":
            importlib.import_module(f"acceptance.checks.{module.stem}")

    modules: set[str] = set()
    inert = False
    for path in changed:
        rel = path.removeprefix(PREFIX)
        if INERT.match(rel):
            inert = True
            continue
        m = re.fullmatch(CHECKS + r"(\w+)\.py", rel)
        if not m or m.group(1) == "__init__" or not (suite / rel).is_file():
            return ""
        modules.add(m.group(1))
    if not modules:
        return "provenance" if inert else ""
    modules = modules_importing(modules, suite / CHECKS)
    ids = sorted(c.id for c in core.REGISTRY if c.func.__module__.rsplit(".", 1)[-1] in modules)
    return ",".join(ids)


if __name__ == "__main__":
    print(select([a for a in sys.argv[1:] if a]))
