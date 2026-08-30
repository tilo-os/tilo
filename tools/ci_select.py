#!/usr/bin/env python3
"""Select and balance the workspace packages affected by a git change.

The selector works from Cargo metadata rather than a hand-maintained package
list.  A package is selected when its own files change, or when it transitively
depends on a changed workspace package.  Repository-wide build inputs fall back
to the complete workspace.  The emitted two-shard matrix is consumed directly
by Forgejo Actions.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
from typing import Iterable


ALWAYS_PACKAGES = {"collab-types", "pkg", "qdoc"}
GLOBAL_FILES = {
    "Cargo.toml",
    "Cargo.lock",
    "clippy.toml",
    "rust-toolchain.toml",
    "deny.toml",
    "os/Containerfile.builder",
}
GLOBAL_PREFIXES = (
    ".cargo/",
    ".config/",
    ".forgejo/",
    "scripts/",
    "tools/",
    "vendor/",
)
HEAVY_PACKAGES = {
    "browser",
    "ebook",
    "media",
    "qdoc-pdf",
    "shell",
    "slides",
    "tube",
    "typst-editor",
    "ui",
    "video-editor",
    "word-processor",
}


def run(*args: str, cwd: Path) -> str:
    return subprocess.check_output(args, cwd=cwd, text=True).strip()


def cargo_metadata(root: Path) -> dict:
    return json.loads(run("cargo", "metadata", "--no-deps", "--format-version", "1", cwd=root))


def package_maps(metadata: dict, root: Path) -> tuple[dict[str, dict], dict[Path, str]]:
    packages = {p["name"]: p for p in metadata["packages"]}
    by_dir = {
        Path(p["manifest_path"]).resolve().parent.relative_to(root.resolve()): p["name"]
        for p in metadata["packages"]
    }
    return packages, by_dir


def changed_files(root: Path, base: str | None, head: str) -> list[str]:
    if not base:
        base = run("git", "rev-parse", f"{head}^", cwd=root)
    try:
        merge_base = run("git", "merge-base", base, head, cwd=root)
    except subprocess.CalledProcessError:
        # Shallow/manual checkouts must be safe.  Selecting everything is much
        # better than silently omitting a package.
        return ["Cargo.toml"]
    output = run("git", "diff", "--name-only", "--diff-filter=ACDMRTUXB", f"{merge_base}...{head}", cwd=root)
    return [line for line in output.splitlines() if line]


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


def package_cost(pkg: dict) -> int:
    """Stable relative cost used only to balance shards.

    CI records real elapsed/user/system time for every shard.  This structural
    score avoids pretending that stale hand-entered timings are measurements,
    while separating known heavy linkers and accounting for targets and direct
    dependencies.
    """
    targets = len(pkg.get("targets", []))
    local_and_external_deps = len(pkg.get("dependencies", []))
    score = 10 + targets * 3 + min(local_and_external_deps, 40)
    if pkg["name"] in HEAVY_PACKAGES:
        score *= 4
    return score


def balance(names: set[str], packages: dict[str, dict], shard_count: int) -> list[dict]:
    shards = [{"names": [], "cost": 0} for _ in range(max(1, min(shard_count, len(names))))]
    for name in sorted(names, key=lambda n: (-package_cost(packages[n]), n)):
        shard = min(shards, key=lambda item: (item["cost"], len(item["names"])))
        shard["names"].append(name)
        shard["cost"] += package_cost(packages[name])
    result = []
    for index, shard in enumerate(shards, 1):
        names_sorted = sorted(shard["names"])
        result.append(
            {
                "shard": index,
                "packages": " ".join(f"-p {name}" for name in names_sorted),
                "package_count": len(names_sorted),
                "estimated_cost": shard["cost"],
            }
        )
    return result


def make_plan(metadata: dict, root: Path, changes: list[str], shard_count: int) -> dict:
    packages, by_dir = package_maps(metadata, root)
    global_change = any(path in GLOBAL_FILES or path.startswith(GLOBAL_PREFIXES) for path in changes)
    unmapped_workspace_change = any(
        (path.startswith("apps/") or path.startswith("crates/") or path.startswith("fs/") or path.startswith("shell/") or path.startswith("ui/"))
        and owner_for(path, by_dir) is None
        for path in changes
    )
    if global_change or unmapped_workspace_change:
        selected = set(packages)
        reason = "repository-wide build input changed" if global_change else "workspace path has no package owner"
        full = True
    else:
        seeds = {owner for path in changes if (owner := owner_for(path, by_dir))}
        selected = closure(seeds, reverse_dependencies(packages, root))
        selected.update(ALWAYS_PACKAGES & packages.keys())
        reason = f"{len(seeds)} changed packages, their reverse dependencies, and contract packages"
        full = False
    return {
        "full": full,
        "reason": reason,
        "changed_files": changes,
        "selected_count": len(selected),
        "workspace_count": len(packages),
        "selected": sorted(selected),
        "matrix": {"include": balance(selected, packages, shard_count)},
    }


def emit_github(plan: dict, output: Path) -> None:
    compact_matrix = json.dumps(plan["matrix"], separators=(",", ":"))
    with output.open("a", encoding="utf-8") as handle:
        handle.write(f"matrix={compact_matrix}\n")
        handle.write(f"full={'true' if plan['full'] else 'false'}\n")
        handle.write(f"selected_count={plan['selected_count']}\n")
        handle.write(f"workspace_count={plan['workspace_count']}\n")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--base", help="base commit/ref (defaults to HEAD^)")
    parser.add_argument("--head", default="HEAD")
    parser.add_argument("--shards", type=int, default=2)
    parser.add_argument("--github-output", type=Path)
    parser.add_argument("--metadata", type=Path, help="read metadata JSON instead of invoking Cargo")
    parser.add_argument("--changes", type=Path, help="read changed paths instead of invoking git")
    args = parser.parse_args()

    root = Path(run("git", "rev-parse", "--show-toplevel", cwd=Path.cwd()))
    metadata = json.loads(args.metadata.read_text()) if args.metadata else cargo_metadata(root)
    changes = args.changes.read_text().splitlines() if args.changes else changed_files(root, args.base, args.head)
    plan = make_plan(metadata, root, changes, args.shards)
    print(json.dumps(plan, indent=2))
    if args.github_output:
        emit_github(plan, args.github_output)
    return 0


if __name__ == "__main__":
    sys.exit(main())
