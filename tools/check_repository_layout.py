#!/usr/bin/env python3
"""Reject generated artifacts and retired source roots tracked in the repository."""

from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path, PurePosixPath


GENERATED_DIRECTORIES = frozenset(
    {
        ".pytest_cache",
        "__pycache__",
        "node_modules",
        "target",
    }
)

RETIRED_SOURCE_ROOTS = {
    "crates/system/windows-vm/": "crates/windows/windows-vm/",
    "fs/": "crates/system/fs/",
    "plan/": "docs/architecture/",
}


def generated_artifact_violations(paths: list[str]) -> list[str]:
    violations = []
    for path in paths:
        parts = PurePosixPath(path).parts
        generated = next((part for part in parts[:-1] if part in GENERATED_DIRECTORIES), None)
        if generated is not None:
            violations.append(f"tracked file is inside generated `{generated}` directory: {path}")
    return sorted(violations)


def retired_source_root_violations(paths: list[str]) -> list[str]:
    violations = []
    for path in paths:
        for retired, replacement in RETIRED_SOURCE_ROOTS.items():
            if path.startswith(retired):
                violations.append(
                    f"tracked file uses retired source root `{retired}`; move it under `{replacement}`: {path}"
                )
                break
    return sorted(violations)


def repository_layout_violations(paths: list[str]) -> list[str]:
    return sorted(generated_artifact_violations(paths) + retired_source_root_violations(paths))


def tracked_files(root: Path) -> list[str]:
    result = subprocess.run(
        ["git", "ls-files", "-z"],
        cwd=root,
        check=True,
        capture_output=True,
    )
    return [
        path.decode("utf-8", errors="surrogateescape")
        for path in result.stdout.split(b"\0")
        if path
    ]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    args = parser.parse_args(argv)
    root = args.root.resolve()
    try:
        violations = repository_layout_violations(tracked_files(root))
    except (OSError, subprocess.CalledProcessError) as error:
        print(f"repository layout check failed: {error}", file=sys.stderr)
        return 1
    if violations:
        for violation in violations:
            print(f"  {violation}", file=sys.stderr)
        print("FAIL: repair the repository layout violations above", file=sys.stderr)
        return 1
    print("repository layout OK (no generated artifacts or retired source roots)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
