#!/usr/bin/env python3
"""Shared Cargo dependency graph and Git change reader for checks and releases.

Changes here invalidate release selection and coordinator fingerprints.
"""
from __future__ import annotations

import json
from pathlib import Path
import re
import subprocess
import tomllib
from typing import Iterable, NamedTuple


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

def file_at(root: Path, revision: str, path: str) -> bytes | None:
    """Return a committed file's bytes, or None when it is absent or unreadable."""
    result = subprocess.run(
        ["git", "show", f"{revision}:{path}"],
        cwd=root, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
    )
    return None if result.returncode else result.stdout


LockId = tuple[str, str, str]


class LockfileChange(NamedTuple):
    """Package names affected by a Cargo.lock difference.

    ``seeds`` are workspace packages whose own locked record changed or which
    depend directly on a changed external record. ``reached`` names every
    locked package (workspace or external) whose dependency graph reaches a
    changed record in either lockfile.
    """
    seeds: frozenset[str]
    reached: frozenset[str]


def _lock_graph(raw: bytes) -> tuple[dict, dict[LockId, tuple]]:
    """Parse Cargo.lock into its non-package keys and normalized package records.

    Dependency strings are resolved to exact package ids, so a second version
    of a crate turning ``"syn"`` into ``"syn 2.0.1"`` does not count as a
    change to every consumer. Unresolvable or ambiguous references raise
    ValueError: the caller must not guess.
    """
    parsed = tomllib.loads(raw.decode("utf-8"))
    entries = parsed.pop("package", [])
    if not isinstance(entries, list):
        raise ValueError("Cargo.lock package table is not a list")
    by_name: dict[str, list[LockId]] = {}
    raw_records: dict[LockId, dict] = {}
    for entry in entries:
        name, version = entry["name"], entry["version"]
        if not isinstance(name, str) or not isinstance(version, str):
            raise ValueError("Cargo.lock package name/version must be strings")
        package_id = (name, version, entry.get("source", ""))
        if package_id in raw_records:
            raise ValueError(f"duplicate Cargo.lock package {package_id}")
        raw_records[package_id] = entry
        by_name.setdefault(name, []).append(package_id)

    def resolve(spec: str) -> LockId:
        match = re.fullmatch(r"(\S+)(?: (\S+))?(?: \((.+)\))?", spec)
        if not match:
            raise ValueError(f"unparseable Cargo.lock dependency {spec!r}")
        name, version, source = match.groups()
        candidates = [
            package_id for package_id in by_name.get(name, [])
            if (version is None or package_id[1] == version)
            and (source is None or package_id[2] == source)
        ]
        if len(candidates) != 1:
            raise ValueError(f"Cargo.lock dependency {spec!r} matches {len(candidates)} packages")
        return candidates[0]

    records = {}
    for package_id, entry in raw_records.items():
        dependencies = entry.get("dependencies", [])
        if not isinstance(dependencies, list):
            raise ValueError(f"Cargo.lock dependencies of {package_id} are not a list")
        rest = tuple(sorted(
            (key, json.dumps(value, sort_keys=True)) for key, value in entry.items()
            if key != "dependencies"
        ))
        records[package_id] = (rest, frozenset(resolve(spec) for spec in dependencies))
    return parsed, records


def lockfile_change(before: bytes | None, after: bytes | None,
                    workspace: Iterable[str]) -> LockfileChange | None:
    """Attribute a Cargo.lock difference to the packages whose inputs it changes.

    Cargo writes the lockfile from a resolve with every feature, target and
    dependency kind (normal, build and dev) enabled, so its graph is a superset
    of what any single build uses. A changed, added or removed record is
    propagated to its consumers in both the old and the new graph: additions
    reach new consumers and removals reach former ones.

    ``None`` means the difference cannot be attributed (missing or malformed
    file, or a change outside the package records). Callers must then treat
    the lockfile as a repository-wide input.
    """
    if before is None or after is None:
        return None
    try:
        old_meta, old = _lock_graph(before)
        new_meta, new = _lock_graph(after)
    except (KeyError, TypeError, ValueError, UnicodeDecodeError):
        return None
    if old_meta != new_meta:
        return None
    changed = {package_id for package_id in old.keys() | new.keys()
               if old.get(package_id) != new.get(package_id)}
    reverse: dict[LockId, set[LockId]] = {}
    for graph in (old, new):
        for consumer, (_, dependencies) in graph.items():
            for dependency in dependencies:
                reverse.setdefault(dependency, set()).add(consumer)
    reached: set[LockId] = set(changed)
    pending = list(changed)
    while pending:
        for consumer in reverse.get(pending.pop(), ()):
            if consumer not in reached:
                reached.add(consumer)
                pending.append(consumer)
    workspace = set(workspace)
    local = {package_id for package_id in old.keys() | new.keys()
             if package_id[0] in workspace and not package_id[2]}
    external_reached = reached - local
    seeds = {
        package_id[0] for package_id in local
        if package_id in changed
        or any(dependency in external_reached
               for graph in (old, new) if package_id in graph
               for dependency in graph[package_id][1])
    }
    return LockfileChange(frozenset(seeds), frozenset(name for name, _, _ in reached))


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
