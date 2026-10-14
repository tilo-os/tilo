#!/usr/bin/env python3
"""Which checks a change to the suite touches, as an `--only` value for run.py.

    scripts/vm-acceptance/ci_select.py <changed path>...

Prints the check ids defined in the changed check modules (and in the modules that
import from them), `provenance` when only documentation or unit tests changed, or
nothing when anything else changed (the runner, the VM door, the report): then the
whole suite runs. P01/P02 come with any selection (run.py adds them).

Paths outside this directory select by what they feed (FEATURES): the input path, the
device-profile fixtures and the in-session agent door each have an area of checks that
proves them in a real session. Anything else outside it is ignored, so the caller may
pass a pull request's whole diff.
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


WORKFLOW = ".forgejo/workflows/vm-acceptance.yml"
# Source paths and the checks that prove them in a session. `input` is the device-profile
# area; B04 (layout) and W17 (typing into Terminal) are the older checks on the same path.
INPUT = "input,B04,W17"
FEATURES = [
    (re.compile(r"^crates/dev/tilo-vm[a-z-]*/"), INPUT),
    (re.compile(r"^crates/session/shell/src/compositor/(input|client_input\.rs|lock_|backend/drm/)"), INPUT),
    (re.compile(r"^os/usr/lib/udev/"), INPUT),
    (re.compile(r"^os/hardware/profiles/lenovo-83ed/input/"), INPUT),
    (re.compile(r"^crates/runtime/tilo-runtime/src/(session_door|mcp|daemon|cli)"), "agent"),
    (re.compile(r"^crates/session/shell/src/(compositor/agent_door|shell/agent_admission)"), "agent"),
    # The guest-side MCP client belongs to the agent checks that run it.
    (re.compile(r"^scripts/vm-acceptance/guest/mcp_client\.py$"), "agent"),
]


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
    features: list[str] = []
    for path in changed:
        if path == WORKFLOW:
            return ""
        feature = next((ids for pattern, ids in FEATURES if pattern.match(path)), None)
        if feature:
            features += feature.split(",")
            continue
        if not path.startswith(PREFIX):
            continue
        rel = path.removeprefix(PREFIX)
        if INERT.match(rel):
            inert = True
            continue
        m = re.fullmatch(CHECKS + r"(\w+)\.py", rel)
        if not m or m.group(1) == "__init__" or not (suite / rel).is_file():
            return ""
        modules.add(m.group(1))
    ids: set[str] = set(features)
    if modules:
        modules = modules_importing(modules, suite / CHECKS)
        ids |= {c.id for c in core.REGISTRY if c.func.__module__.rsplit(".", 1)[-1] in modules}
    if not ids:
        # Nothing of the suite's own changed and nothing it proves: the run itself, or all.
        return "provenance" if inert else ""
    return ",".join(sorted(ids))


if __name__ == "__main__":
    print(select([a for a in sys.argv[1:] if a]))
