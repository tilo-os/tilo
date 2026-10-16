#!/usr/bin/env python3
"""Ensure path-filtered workflows and t checks follow workspace dependency closures.

A workflow's `paths` lists decide when it runs; a t check's `inputs` decide when its cached
result is stale. Either must cover every directory of the package's dependency closure. Name a
t check as `t.toml#<check>`. A t check that names the package in `cargo_packages` covers it by
construction: t derives those inputs from the manifests and the lockfile.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
from fnmatch import fnmatchcase
import json
from pathlib import Path
import re
import subprocess
import sys
import tomllib
from typing import Any


ROOT = Path(__file__).resolve().parents[2]
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


def _resolve_graph_directories(
    metadata: dict[str, Any], root: Path, package: str
) -> set[str]:
    """Fixture-friendly closure over Cargo's unified resolve graph."""
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


def _cargo_tree_directories(
    metadata: dict[str, Any], root: Path, package: str
) -> set[str]:
    """Directories linked by a normal build of ``package`` (no dev/build edges).

    Workspace feature unification makes ``cargo metadata``'s resolve graph pull
    optional and test-only crates into packages that a release binary does not
    link. ``cargo tree --edges normal`` matches ``cargo build -p`` / image
    Containerfiles, so path filters rebuild when the image's inputs change and
    stay quiet for pure test-harness edges. Both Linux architectures are
    named, so the graph doesn't depend on the machine resolving it.
    """
    output = subprocess.check_output(
        [
            "cargo",
            "tree",
            "-p",
            package,
            "--edges",
            "normal",
            "--prefix",
            "none",
            "--locked",
            "--target",
            "x86_64-unknown-linux-gnu",
            "--target",
            "aarch64-unknown-linux-gnu",
        ],
        cwd=root,
        text=True,
        stderr=subprocess.DEVNULL,
    )
    names = {line.split()[0] for line in output.splitlines() if line.strip()}
    return {
        Path(item["manifest_path"]).parent.relative_to(root).as_posix()
        for item in metadata["packages"]
        if item["name"] in names
        and Path(item["manifest_path"]).is_relative_to(root)
    }


def dependency_directories(
    metadata: dict[str, Any], root: Path, package: str
) -> set[str]:
    roots = [item["id"] for item in metadata["packages"] if item["name"] == package]
    if len(roots) != 1:
        raise ValueError(
            f"expected one workspace package named {package}, found {len(roots)}"
        )
    # Real workspace package ids are ``path+file://...``. Compact fixture ids
    # keep the resolve-graph walk so unit tests do not need a Cargo tree.
    if not str(roots[0]).startswith("path+file://"):
        return _resolve_graph_directories(metadata, root, package)
    return _cargo_tree_directories(metadata, root, package)


def pattern_covers(directory: str, pattern: str) -> bool:
    """Whether changing a package directory would activate ``pattern``."""
    return fnmatchcase(directory, pattern) or fnmatchcase(
        f"{directory}/Cargo.toml", pattern
    )


def unmatched_patterns(workflow_text: str, files: list[str]) -> list[str]:
    """Trigger patterns no tracked file matches: a moved or deleted input whose
    changes would no longer start the workflow."""
    return sorted(
        pattern
        for pattern in set().union(*workflow_path_blocks(workflow_text))
        if not pattern.startswith("!")
        and not any(fnmatchcase(path, pattern) for path in files)
    )


def t_check_blocks(text: str, name: str, package: str) -> list[set[str]]:
    """A t check's `inputs` as one paths block; none to audit when its `cargo_packages`
    names `package`, whose dependency closure t then derives itself."""
    for check in tomllib.loads(text).get("check", []):
        if check.get("name") == name:
            packages = check.get("cargo_packages", [])
            if any(fnmatchcase(package, pattern) for pattern in packages):
                return []
            # As t reads it: without `inputs`, everything, unless it names packages instead.
            return [set(check.get("inputs", [] if packages else ["**"]))]
    raise ValueError(f"t.toml declares no check {name}")


def audit(
    metadata: dict[str, Any],
    root: Path,
    package: str,
    workflow_text: str,
    blocks: list[set[str]] | None = None,
) -> list[Violation]:
    directories = dependency_directories(metadata, root, package)
    if blocks is None:
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
    parser.add_argument("package", nargs="?")
    parser.add_argument("workflow", nargs="?", type=Path)
    parser.add_argument(
        "--check",
        action="append",
        nargs=2,
        metavar=("PACKAGE", "WORKFLOW"),
        default=[],
        help="audit one package/workflow pair; repeat to share one metadata snapshot",
    )
    args = parser.parse_args()
    checks = [(package, Path(workflow)) for package, workflow in args.check]
    if args.package is not None or args.workflow is not None:
        if args.package is None or args.workflow is None:
            parser.error("the positional package and workflow must be supplied together")
        checks.insert(0, (args.package, args.workflow))
    if not checks:
        parser.error("supply a package/workflow pair or at least one --check")
    try:
        metadata = load_metadata(ROOT)
        results = []
        for package, workflow in checks:
            path, _, check = str(workflow).partition("#")
            text = (ROOT / path).read_text(encoding="utf-8")
            blocks = t_check_blocks(text, check, package) if check else None
            results.append((package, workflow, audit(metadata, ROOT, package, text, blocks)))
    except (OSError, subprocess.CalledProcessError, ValueError) as error:
        print(error, file=sys.stderr)
        return 2
    failed = [(package, workflow, violations) for package, workflow, violations in results if violations]
    if failed:
        for package, workflow, violations in failed:
            print(
                f"{workflow}: workflow misses {package} dependency paths:",
                file=sys.stderr,
            )
            print("\n".join(str(item) for item in violations), file=sys.stderr)
        return 1
    for package, workflow, _ in results:
        print(f"{workflow}: every paths block tracks {package}'s dependency closure")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
