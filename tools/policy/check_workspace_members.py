#!/usr/bin/env python3
"""Validate that the root Cargo workspace has one entry per repository crate.

Every member is also on the workspace's one edition, ``EDITION``: a crate on an
older edition compiles, so nothing else would notice it.
"""

from __future__ import annotations

import argparse
import sys
import tomllib
from collections import Counter
from pathlib import Path
from typing import NamedTuple

EDITION = "2024"


class WorkspaceInventory(NamedTuple):
    members: tuple[str, ...]
    excluded: frozenset[str]
    discovered: frozenset[str]


def load_manifest(path: Path) -> dict:
    with path.open("rb") as handle:
        return tomllib.load(handle)


def string_list(document: dict, key: str) -> tuple[str, ...]:
    workspace = document.get("workspace")
    if not isinstance(workspace, dict):
        raise ValueError("Cargo.toml has no [workspace] table")
    values = workspace.get(key, [])
    if not isinstance(values, list) or not all(isinstance(value, str) for value in values):
        raise ValueError(f"[workspace].{key} must be a list of paths")
    return tuple(values)


def discover_crates(root: Path, excluded: frozenset[str]) -> frozenset[str]:
    crates: set[str] = set()
    for top_level in ("apps", "crates", "examples"):
        directory = root / top_level
        if not directory.is_dir():
            continue
        for manifest_path in directory.rglob("Cargo.toml"):
            relative = manifest_path.parent.relative_to(root).as_posix()
            if relative in excluded:
                continue
            document = load_manifest(manifest_path)
            if "workspace" in document:
                continue
            crates.add(relative)
    return frozenset(crates)


def member_edition(root: Path, member: str) -> str:
    """The edition a member's manifest declares, inherited or not; Cargo's default is 2015."""
    edition = load_manifest(root / member / "Cargo.toml").get("package", {}).get("edition", "2015")
    if isinstance(edition, dict) and edition.get("workspace") is True:
        workspace = load_manifest(root / "Cargo.toml").get("workspace", {})
        edition = workspace.get("package", {}).get("edition", "2015")
    return str(edition)


def inventory(root: Path) -> WorkspaceInventory:
    document = load_manifest(root / "Cargo.toml")
    members = string_list(document, "members")
    excluded = frozenset(string_list(document, "exclude"))
    return WorkspaceInventory(
        members=members,
        excluded=excluded,
        discovered=discover_crates(root, excluded),
    )


def violations(root: Path, state: WorkspaceInventory) -> list[str]:
    errors: list[str] = []
    counts = Counter(state.members)
    for member, count in sorted(counts.items()):
        if count > 1:
            errors.append(f"duplicate workspace member ({count} entries): {member}")

    declared = frozenset(state.members)
    for crate in sorted(state.discovered - declared):
        errors.append(f"crate is not a workspace member: {crate}")
    for member in sorted(declared):
        if not (root / member / "Cargo.toml").is_file():
            errors.append(f"workspace member has no Cargo.toml: {member}")
        elif (edition := member_edition(root, member)) != EDITION:
            errors.append(
                f"workspace member is on edition {edition}, not {EDITION}: {member}"
            )
    for member in sorted(declared & state.excluded):
        errors.append(f"path is both a workspace member and excluded: {member}")
    return errors


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    args = parser.parse_args(argv)
    root = args.root.resolve()
    try:
        state = inventory(root)
        errors = violations(root, state)
    except (OSError, tomllib.TOMLDecodeError, ValueError) as error:
        print(f"workspace membership check failed: {error}", file=sys.stderr)
        return 1
    if errors:
        for error in errors:
            print(f"  {error}", file=sys.stderr)
        print(
            f"FAIL: repair [workspace].members in Cargo.toml; every member is edition {EDITION}",
            file=sys.stderr,
        )
        return 1
    print(
        "workspace membership OK "
        f"({len(state.members)} unique members; {len(state.discovered)} repository crates)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
