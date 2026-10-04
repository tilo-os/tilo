#!/usr/bin/env python3
"""Whether a change can affect what the app SDK lanes build.

The lanes build tilo-app's CLIs and every app under apps/ (Linux, web). A change
matters when it touches one of those packages or a workspace package they
depend on, the workflow itself, or a repository-wide build input. Changes only
to packages outside that closure (the shell, OS services, tools) skip the lanes.

Prints `affected=true|false` and, with --github-output, appends it there.
"""

from __future__ import annotations

import argparse
from pathlib import Path
import sys

TOOLS = Path(__file__).resolve().parents[1]
if str(TOOLS) not in sys.path:
    sys.path.append(str(TOOLS))
from ci.affected_packages import resolve_lockfile
from ci.workspace_graph import (
    cargo_metadata, changed_files, closure, comparison_merge_base, file_at, owner_for,
    package_maps, reverse_dependencies,
)

SDK_PACKAGES = {"tilo-app-cli", "tilo-app-package-cli", "tilo-caps"}

# Paths outside any package that change what the lanes build or how they run.
GLOBAL_INPUTS = (
    ".cargo/",
    ".forgejo/workflows/app-sdk.yml",
    ".forgejo/actions/",
    "Cargo.toml",
    "rust-toolchain.toml",
    "tools/ci/app_sdk_affected.py",
    "tools/ci/workspace_graph.py",
    "tools/policy/app_capability",
    "tools/policy/app_portability",
    "tools/policy/check_app_",
    "tools/sdk/",
    "tools/web/",
    "vendor/",
)


def affected(root: Path, changed: list[str], metadata: dict,
             base: str, head: str) -> tuple[bool, str]:
    packages, by_dir = package_maps(metadata, root)
    seeds = {name for name in packages if name in SDK_PACKAGES}
    seeds |= {name for d, name in by_dir.items() if d.parts[:1] == ("apps",)}
    reverse = reverse_dependencies(packages, root)
    for path in changed:
        if path == "Cargo.lock":
            # Attributed to the packages whose locked inputs changed; unattributable
            # differences count as repository-wide.
            lock = resolve_lockfile(file_at(root, base, path), file_at(root, head, path), metadata)
            if lock is None:
                return True, path
            if closure(lock.seeds, reverse) & seeds:
                return True, f"{path} ({', '.join(sorted(lock.seeds)[:3])})"
            continue
        if path.startswith(GLOBAL_INPUTS):
            return True, path
        owner = owner_for(path, by_dir)
        if owner is None:
            # Under apps/ or crates/ but in no package: be safe.
            if path.startswith(("apps/", "crates/")):
                return True, path
            continue
        if closure([owner], reverse) & seeds:
            return True, f"{path} ({owner})"
    return False, ""


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", help="base commit/ref (defaults to HEAD^)")
    parser.add_argument("--head", default="HEAD")
    parser.add_argument("--github-output", type=Path)
    args = parser.parse_args()
    root = Path.cwd()
    base = comparison_merge_base(root, args.base, args.head)
    hit, why = affected(root, changed_files(root, args.base, args.head), cargo_metadata(root),
                        base, args.head)
    print(f"affected={'true' if hit else 'false'}" + (f"  # {why}" if why else ""))
    if args.github_output:
        with args.github_output.open("a") as out:
            out.write(f"affected={'true' if hit else 'false'}\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
