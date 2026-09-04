#!/usr/bin/env python3
"""Ensure a path-filtered workflow tracks a package's workspace dependency closure."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("package")
    parser.add_argument("workflow", type=Path)
    args = parser.parse_args()
    metadata = json.loads(
        subprocess.check_output(
            ["cargo", "metadata", "--format-version", "1"], cwd=ROOT, text=True
        )
    )
    packages = {item["id"]: item for item in metadata["packages"]}
    nodes = {item["id"]: item for item in metadata["resolve"]["nodes"]}
    local_ids = {
        package_id
        for package_id, item in packages.items()
        if Path(item["manifest_path"]).is_relative_to(ROOT)
    }
    roots = [item["id"] for item in metadata["packages"] if item["name"] == args.package]
    if len(roots) != 1:
        print(f"expected one workspace package named {args.package}, found {len(roots)}", file=sys.stderr)
        return 2
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

    workflow = (ROOT / args.workflow).read_text()
    missing: list[str] = []
    for package_id in seen:
        manifest = Path(packages[package_id]["manifest_path"])
        try:
            directory = manifest.parent.relative_to(ROOT)
        except ValueError:
            continue
        pattern = f'- "{directory.as_posix()}/**"'
        if pattern not in workflow:
            missing.append(pattern)
    if missing:
        print("workflow misses package dependency paths:", file=sys.stderr)
        print("\n".join(sorted(missing)), file=sys.stderr)
        return 1
    print(f"{args.workflow}: tracks all {len(seen)} resolved dependencies of {args.package}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
