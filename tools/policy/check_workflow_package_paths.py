#!/usr/bin/env python3
"""Ensure path-filtered workflows follow workspace dependency closures.

A workflow's `paths` lists decide when it runs, so they must cover every directory of the
package's dependency closure. The closure entries are generated rather than kept by hand: a
block marks where they stand, `--write` fills them from the package's closure, and the check
fails when a hand-written list or a stale region would let the workflow be skipped.
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
from collections.abc import Iterable, Sequence
from typing import Any


ROOT = Path(__file__).resolve().parents[2]
# The platforms a build is resolved for: both Linux architectures, so the graph doesn't depend on
# the machine resolving it.
LINUX_TARGETS = ("x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu")
# The Cargo edges a workflow's build follows: `normal` matches a release binary, and a job that
# also builds and tests (`--all-targets`) adds `build` and `dev`.
DEFAULT_EDGES = "normal"
PATHS_KEY = re.compile(r"^(?P<indent>\s*)paths:\s*(?:#.*)?$")
LIST_ITEM = re.compile(r"^\s*-\s*(?P<value>.+?)\s*$")
# The closure entries stand between these two comment lines, so the generator owns them alone.
GENERATED_BEGIN = (
    "# Generated: the package's dependency closure "
    "(tools/policy/check_workflow_package_paths.py --write)"
)
GENERATED_END = "# End generated."


@dataclass(frozen=True)
class Violation:
    block: int
    message: str

    def __str__(self) -> str:
        return f"paths block {self.block} {self.message}"


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


def paths_regions(text: str) -> list[tuple[int, int, int]]:
    """Each ``paths`` list as its key's indent and its item lines' half-open range."""
    regions: list[tuple[int, int, int]] = []
    current: tuple[int, int] | None = None
    for index, raw in enumerate(text.splitlines()):
        key = PATHS_KEY.match(raw)
        if key:
            if current is not None:
                regions.append((current[0], current[1], index))
            current = (len(key.group("indent")), index + 1)
            continue
        if current is None:
            continue
        stripped = raw.strip()
        indent = len(raw) - len(raw.lstrip())
        if stripped and not stripped.startswith("#") and indent <= current[0]:
            regions.append((current[0], current[1], index))
            current = None
    if current is not None:
        regions.append((current[0], current[1], len(text.splitlines())))
    return regions


def closure_items(directories: Iterable[str], indent: str) -> list[str]:
    """The generated ``paths`` entries for a closure, in the list's own indentation."""
    return [f'{indent}- "{directory}/**"\n' for directory in sorted(directories)]


def regenerate(
    text: str,
    directories: set[str],
    report_drift: bool = True,
    edges: str = DEFAULT_EDGES,
) -> tuple[str, list[Violation]]:
    """``text`` with every block's generated region filled from the closure.

    Also reports what stands in the way of generating it, and, unless
    ``report_drift`` is off because the caller is about to write it, a region
    that does not hold the closure.
    """
    lines = text.splitlines(keepends=True)
    edits: list[tuple[int, int, list[str]]] = []
    violations: list[Violation] = []
    for number, (key_indent, start, end) in enumerate(paths_regions(text), start=1):
        begins = [i for i in range(start, end) if lines[i].strip() == GENERATED_BEGIN]
        ends = [i for i in range(start, end) if lines[i].strip() == GENERATED_END]
        if len(begins) != 1 or len(ends) != 1 or ends[0] < begins[0]:
            violations.append(
                Violation(
                    number,
                    f"needs one {GENERATED_BEGIN!r} line and one {GENERATED_END!r} line "
                    "around its generated closure",
                )
            )
            continue
        for index in (i for i in range(start, end) if not begins[0] < i < ends[0]):
            written = lines[index].strip()
            if written.startswith("-") and "**" in written:
                violations.append(
                    Violation(
                        number,
                        f"keeps {written!r} by hand outside the generated region",
                    )
                )
        items = closure_items(directories, " " * (key_indent + 2))
        held = [lines[i].strip() for i in range(begins[0] + 1, ends[0]) if lines[i].strip()]
        if report_drift and held != [item.strip() for item in items]:
            violations.append(
                Violation(
                    number,
                    f"generated closure is not current: run tools/policy/"
                    f"check_workflow_package_paths.py --edges {edges} --write <package> <workflow>",
                )
            )
        edits.append((begins[0] + 1, ends[0], items))
    for first, last, items in reversed(edits):
        lines[first:last] = items
    return "".join(lines), violations


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
    metadata: dict[str, Any], root: Path, package: str, cargo_args: Sequence[str] = (),
    targets: Sequence[str] = LINUX_TARGETS, edges: str = DEFAULT_EDGES,
) -> set[str]:
    """Directories linked by a build of ``package`` on ``edges``.

    Workspace feature unification makes ``cargo metadata``'s resolve graph pull
    optional and test-only crates into packages that a release binary does not
    link. ``cargo tree`` on the edges the workflow's own build follows (``cargo
    build`` and image Containerfiles: ``normal``) makes the filters rebuild when
    the build's inputs change and stay quiet for edges it never links.
    ``targets`` are the platforms it builds for, and ``cargo_args`` the features
    the image builds with, such as ``--no-default-features``.
    """
    output = subprocess.check_output(
        [
            "cargo",
            "tree",
            "-p",
            package,
            "--edges",
            edges,
            "--prefix",
            "none",
            "--locked",
            *(arg for target in targets for arg in ("--target", target)),
            *cargo_args,
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
    metadata: dict[str, Any], root: Path, package: str, cargo_args: Sequence[str] = (),
    targets: Sequence[str] = LINUX_TARGETS, edges: str = DEFAULT_EDGES,
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
    return _cargo_tree_directories(metadata, root, package, cargo_args, targets, edges)


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


def audit(
    metadata: dict[str, Any],
    root: Path,
    package: str,
    workflow_text: str,
    blocks: list[set[str]] | None = None,
    cargo_args: Sequence[str] = (),
    targets: Sequence[str] = LINUX_TARGETS,
    edges: str = DEFAULT_EDGES,
) -> list[Violation]:
    directories = dependency_directories(metadata, root, package, cargo_args, targets, edges)
    if blocks is None:
        blocks = workflow_path_blocks(workflow_text)
    if not blocks:
        return []
    violations = [
        Violation(index, f'misses - "{directory}/**"')
        for index, patterns in enumerate(blocks, start=1)
        for directory in sorted(directories)
        if not any(pattern_covers(directory, pattern) for pattern in patterns)
    ]
    return violations + regenerate(workflow_text, directories, edges=edges)[1]


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
    parser.add_argument(
        "--write",
        action="store_true",
        help="fill each workflow's generated paths from its package's closure",
    )
    parser.add_argument(
        "--edges",
        default=DEFAULT_EDGES,
        help=f"the Cargo edges the workflow's build follows (default {DEFAULT_EDGES})",
    )
    args = parser.parse_args()
    checks = [(package, Path(workflow)) for package, workflow in args.check]
    if args.package is not None or args.workflow is not None:
        if args.package is None or args.workflow is None:
            parser.error("the positional package and workflow must be supplied together")
        checks.insert(0, (args.package, args.workflow))
    if not checks:
        parser.error("supply a package/workflow pair or at least one --check")
    results: list[tuple[Path, list[Violation]]] = []
    try:
        metadata = load_metadata(ROOT)
        for package, workflow in checks:
            text = (ROOT / workflow).read_text(encoding="utf-8")
            if args.write:
                directories = dependency_directories(
                    metadata, ROOT, package, edges=args.edges
                )
                regenerated, violations = regenerate(
                    text, directories, report_drift=False, edges=args.edges
                )
                if regenerated != text:
                    (ROOT / workflow).write_text(regenerated, encoding="utf-8")
                    print(f"{workflow}: generated {len(directories)} dependency paths")
                results.append((workflow, violations))
            else:
                results.append(
                    (workflow, audit(metadata, ROOT, package, text, edges=args.edges))
                )
    except (OSError, subprocess.CalledProcessError, ValueError) as error:
        print(error, file=sys.stderr)
        return 2
    failed = [(workflow, violations) for workflow, violations in results if violations]
    if failed:
        for workflow, violations in failed:
            for item in violations:
                print(f"{workflow}: {item}")
        return 1
    for workflow, _ in results:
        print(f"{workflow}: every paths block holds the closure", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
