#!/usr/bin/env python3
"""Shared Cargo dependency graph and Git change reader for checks and releases.

Keep runner scheduling and test policy in affected_packages.py. Changes here
invalidate release selection and coordinator fingerprints.
"""
from __future__ import annotations

import json
from pathlib import Path
import subprocess
from typing import Iterable


def run(*args: str, cwd: Path) -> str:
    return subprocess.check_output(args, cwd=cwd, text=True).strip()

def cargo_metadata(root: Path) -> dict:
    return json.loads(run("cargo", "metadata", "--locked", "--no-deps", "--format-version", "1", cwd=root))

def package_maps(metadata: dict, root: Path) -> tuple[dict[str, dict], dict[Path, str]]:
    packages = {p["name"]: p for p in metadata["packages"]}
    by_dir = {
        Path(p["manifest_path"]).resolve().parent.relative_to(root.resolve()): p["name"]
        for p in metadata["packages"]
    }
    return packages, by_dir

def comparison_merge_base(root: Path, base: str | None, head: str) -> str:
    if not base:
        base = run("git", "rev-parse", f"{head}^", cwd=root)
    try:
        return run("git", "merge-base", base, head, cwd=root)
    except subprocess.CalledProcessError as error:
        raise RuntimeError(
            f"cannot determine merge base for {base!r} and {head!r}; "
            "refusing to select packages from a two-tree diff"
        ) from error

def changed_files(root: Path, base: str | None, head: str) -> list[str]:
    merge_base = comparison_merge_base(root, base, head)
    # Compare the PR side with the common ancestor. A direct base..head diff
    # would also report files changed only on an advanced base branch.
    diff_range = f"{merge_base}..{head}"
    # A move affects both the old owner and the new owner. Rename detection's
    # name-only output reports only the destination, losing the old package's
    # reverse dependants. NUL delimiters also avoid Git quoting Unicode paths
    # or splitting valid filenames containing newlines/leading whitespace.
    output = subprocess.check_output(
        ["git", "diff", "--no-renames", "--name-only", "-z",
         "--diff-filter=ACDMRTUXB", diff_range, "--"], cwd=root,
    )
    return [path.decode("utf-8", errors="surrogateescape")
            for path in output.split(b"\0") if path]

def owner_for(path: str, by_dir: dict[Path, str]) -> str | None:
    candidate = Path(path)
    owners = ((len(directory.parts), name) for directory, name in by_dir.items() if candidate.is_relative_to(directory))
    return max(owners, default=(0, None))[1]

def reverse_dependencies(packages: dict[str, dict], root: Path) -> dict[str, set[str]]:
    manifest_to_name = {
        str(Path(pkg["manifest_path"]).resolve()): name for name, pkg in packages.items()
    }
    reverse = {name: set() for name in packages}
    for consumer, pkg in packages.items():
        for dependency in pkg.get("dependencies", []):
            dep_path = dependency.get("path")
            if not dep_path:
                continue
            manifest = str((Path(dep_path).resolve() / "Cargo.toml"))
            provider = manifest_to_name.get(manifest)
            if provider:
                reverse[provider].add(consumer)
    return reverse

def closure(seeds: Iterable[str], reverse: dict[str, set[str]]) -> set[str]:
    selected = set(seeds)
    pending = list(selected)
    while pending:
        package = pending.pop()
        for dependent in reverse.get(package, ()):
            if dependent not in selected:
                selected.add(dependent)
                pending.append(dependent)
    return selected
