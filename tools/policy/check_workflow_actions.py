#!/usr/bin/env python3
"""Reject mutable third-party actions in Forgejo workflow definitions.

Remote actions execute code inside CI and therefore must use an immutable
40-character commit SHA. Repository-local actions and explicit ``docker://``
images are not remote action dependencies and are left to their own source and
image policy checks.
"""

from __future__ import annotations

import argparse
import re
from dataclasses import dataclass
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
USES = re.compile(r"^\s*(?:-\s*)?uses:\s*['\"]?([^'\"#\s]+)")
COMMIT = re.compile(r"^[0-9a-fA-F]{40}$")


@dataclass(frozen=True)
class Violation:
    path: Path
    line: int
    reference: str

    def display(self, root: Path) -> str:
        try:
            path = self.path.relative_to(root)
        except ValueError:
            path = self.path
        return f"{path}:{self.line}: mutable workflow action: {self.reference}"


def workflow_files(directory: Path) -> list[Path]:
    """Return workflows and nested composite-action definitions."""
    if not directory.is_dir():
        raise ValueError(f"workflow directory is missing or not a directory: {directory}")
    paths = sorted((*directory.rglob("*.yml"), *directory.rglob("*.yaml")))
    if not paths:
        raise ValueError(f"no workflow/action definitions found in {directory}")
    return paths


def audit(directory: Path) -> list[Violation]:
    violations: list[Violation] = []
    for path in workflow_files(directory):
        for line_number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
            match = USES.match(line)
            if match is None:
                continue
            reference = match.group(1)
            if reference.startswith(("./", "docker://")):
                continue
            separator, revision = reference.rsplit("@", 1) if "@" in reference else ("", "")
            if not separator or COMMIT.fullmatch(revision) is None:
                violations.append(Violation(path, line_number, reference))
    return violations


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--workflows",
        type=Path,
        default=ROOT / ".forgejo",
        help="Forgejo configuration tree to audit recursively",
    )
    args = parser.parse_args()
    try:
        violations = audit(args.workflows)
    except (OSError, ValueError) as error:
        parser.exit(1, f"workflow action inspection failed: {error}\n")
    for violation in violations:
        print(violation.display(ROOT))
    if violations:
        return 1
    print(
        "workflow action pins OK "
        f"({len(workflow_files(args.workflows))} workflow/action definitions)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
