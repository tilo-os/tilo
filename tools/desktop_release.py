#!/usr/bin/env python3
"""Decide whether a commit range changes the reusable desktop payload."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import sys

import ci_select


ROOT = Path(__file__).resolve().parents[1]
DESKTOP_PACKAGES = {"launcher", "qos-unlock-helper", "shell"}
DIRECT_INPUTS = {
    ".forgejo/workflows/package-desktop.yml",
    "Cargo.lock",
    "Cargo.toml",
    "docs/install-contract.json",
    "flake.lock",
    "flake.nix",
    "rust-toolchain.toml",
    "scripts/check-install-contract.py",
}
DIRECT_PREFIXES = ("dist/",)
WORKSPACE_PREFIXES = ("apps/", "crates/", "fs/", "shell/", "ui/")


def affected_packages(paths: list[str], metadata: dict, root: Path) -> set[str]:
    packages, by_dir = ci_select.package_maps(metadata, root)
    if any(
        path in ci_select.GLOBAL_FILES or path.startswith(ci_select.GLOBAL_PREFIXES)
        for path in paths
    ):
        return set(packages)
    if any(
        path.startswith(WORKSPACE_PREFIXES) and ci_select.owner_for(path, by_dir) is None
        for path in paths
    ):
        # A deleted or newly added workspace package is absent from one side of
        # Cargo metadata. Rebuild conservatively instead of missing a dependency.
        return set(packages)
    seeds = {
        owner
        for path in paths
        if (owner := ci_select.owner_for(path, by_dir)) is not None
    }
    return ci_select.closure(seeds, ci_select.reverse_dependencies(packages, root))


def desktop_changed(
    paths: list[str], metadata: dict | None = None, root: Path = ROOT
) -> bool:
    if any(path in DIRECT_INPUTS or path.startswith(DIRECT_PREFIXES) for path in paths):
        return True
    if not any(
        path.startswith(WORKSPACE_PREFIXES)
        or path.startswith(ci_select.GLOBAL_PREFIXES)
        or path in ci_select.GLOBAL_FILES
        for path in paths
    ):
        return False
    metadata = metadata or ci_select.cargo_metadata(root)
    return bool(DESKTOP_PACKAGES & affected_packages(paths, metadata, root))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--base", help="base commit/ref (defaults to HEAD^)")
    parser.add_argument("--head", default="HEAD")
    parser.add_argument("--github-output", type=Path)
    args = parser.parse_args()
    root = Path(ci_select.run("git", "rev-parse", "--show-toplevel", cwd=Path.cwd()))
    paths = ci_select.changed_files(root, args.base, args.head)
    affected = desktop_changed(paths, root=root)
    result = {"affected": affected, "changed_files": paths}
    print(json.dumps(result, sort_keys=True))
    if args.github_output:
        with args.github_output.open("a", encoding="utf-8") as stream:
            stream.write(f"affected={str(affected).lower()}\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
