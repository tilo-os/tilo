#!/usr/bin/env python3
"""Ensure path-filtered workflows follow workspace dependency closures."""

from __future__ import annotations

import argparse
from dataclasses import dataclass
from fnmatch import fnmatchcase
import json
from pathlib import Path
import re
import subprocess
import sys
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
PATHS_KEY = re.compile(r"^(?P<indent>\s*)paths:\s*(?:#.*)?$")
LIST_ITEM = re.compile(r"^\s*-\s*(?P<value>.+?)\s*$")


@dataclass(frozen=True)
class Violation:
    block: int
    directory: str

    def __str__(self) -> str:
        return f'paths block {self.block} misses - "{self.directory}/**"'


def workflow_path_blocks(text: str) -> list[set[str]]:
    """Return each event ``paths`` list without requiring a YAML dependency."""
    blocks: list[set[str]] = []
    current: set[str] | None = None
    key_indent = -1
    for raw in text.splitlines():
        key = PATHS_KEY.match(raw)
        if key:
            current = set()
            blocks.append(current)
            key_indent = len(key.group("indent"))
            continue
        if current is None:
            continue
        stripped = raw.strip()
        if not stripped or stripped.startswith("#"):
            continue
        indent = len(raw) - len(raw.lstrip())
        if indent <= key_indent:
            current = None
            continue
        item = LIST_ITEM.match(raw)
        if item:
            value = item.group("value").split(" #", 1)[0].strip().strip("'\"")
            if value:
                current.add(value.removeprefix("./"))
    return blocks


def dependency_directories(
    metadata: dict[str, Any], root: Path, package: str
) -> set[str]:
    packages = {item["id"]: item for item in metadata["packages"]}
    nodes = {item["id"]: item for item in metadata["resolve"]["nodes"]}
    local_ids = {
        package_id
        for package_id, item in packages.items()
        if Path(item["manifest_path"]).is_relative_to(root)
    }
    roots = [item["id"] for item in metadata["packages"] if item["name"] == package]
    if len(roots) != 1:
        raise ValueError(
            f"expected one workspace package named {package}, found {len(roots)}"
        )

    seen: set[str] = set()
    pending = roots
    while pending:
        package_id = pending.pop()
        if package_id in seen:
            continue
        seen.add(package_id)
        pending.extend(
            dependency
            for dependency in nodes[package_id]["dependencies"]
            if dependency in local_ids
        )

    return {
        Path(packages[package_id]["manifest_path"])
        .parent.relative_to(root)
        .as_posix()
        for package_id in seen
    }


def pattern_covers(directory: str, pattern: str) -> bool:
    """Whether changing a package directory would activate ``pattern``."""
    return fnmatchcase(directory, pattern) or fnmatchcase(
        f"{directory}/Cargo.toml", pattern
    )


def audit(
    metadata: dict[str, Any], root: Path, package: str, workflow_text: str
) -> list[Violation]:
    directories = dependency_directories(metadata, root, package)
    blocks = workflow_path_blocks(workflow_text)
    if not blocks:
        return []
    return [
        Violation(block=index, directory=directory)
        for index, patterns in enumerate(blocks, start=1)
        for directory in sorted(directories)
        if not any(pattern_covers(directory, pattern) for pattern in patterns)
    ]


def load_metadata(root: Path) -> dict[str, Any]:
    return json.loads(
        subprocess.check_output(
            ["cargo", "metadata", "--locked", "--format-version", "1"],
            cwd=root,
            text=True,
        )
    )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("package")
    parser.add_argument("workflow", type=Path)
    args = parser.parse_args()
    try:
        metadata = load_metadata(ROOT)
        workflow = (ROOT / args.workflow).read_text(encoding="utf-8")
        violations = audit(metadata, ROOT, args.package, workflow)
    except (OSError, subprocess.CalledProcessError, ValueError) as error:
        print(error, file=sys.stderr)
        return 2
    if violations:
        print("workflow misses package dependency paths:", file=sys.stderr)
        print("\n".join(str(item) for item in violations), file=sys.stderr)
        return 1
    print(f"{args.workflow}: every paths block tracks {args.package}'s dependency closure")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
