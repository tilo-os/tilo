#!/usr/bin/env python3
"""Print the revision range a CI check compares: `<base>..HEAD`.

Shared by the changed-file checks (rustfmt, unused dependencies, commit
policy) so they all agree on what "changed" means and all fail when the
comparison is unavailable.
"""

from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path


ZERO_SHA = "0" * 40


def git(root: Path, *arguments: str) -> str:
    return subprocess.check_output(
        ["git", *arguments], cwd=root, text=True, errors="surrogateescape"
    ).strip()


def comparison_range(root: Path, base: str, before: str) -> str:
    """PRs compare their common ancestor; pushes compare the exact previous tree.

    A shallow checkout cannot safely substitute the current target branch tree
    for a missing PR merge base: that would attribute target-only edits to the PR.
    Fetch both immutable endpoints, deepening only when ancestry is missing.
    """
    is_pr = bool(base and base != "null")
    candidate = base if is_pr else before
    if not candidate or candidate in {"null", ZERO_SHA}:
        candidate = git(root, "rev-parse", "HEAD^")
    try:
        subprocess.run(
            ["git", "cat-file", "-e", f"{candidate}^{{commit}}"],
            cwd=root, check=True, capture_output=True,
        )
    except subprocess.CalledProcessError:
        subprocess.run(
            ["git", "fetch", "--no-tags", "--depth=1", "origin", candidate],
            cwd=root, check=True,
        )
    if not is_pr:
        return f"{candidate}..HEAD"

    head = git(root, "rev-parse", "HEAD")
    for deepen in ("--deepen=64", "--deepen=256", "--deepen=1024", "--unshallow", None):
        try:
            merge_base = git(root, "merge-base", candidate, head)
            return f"{merge_base}..HEAD"
        except subprocess.CalledProcessError:
            if deepen is None or git(root, "rev-parse", "--is-shallow-repository") != "true":
                raise
        subprocess.run(
            ["git", "fetch", "--no-tags", deepen, "origin", candidate, head],
            cwd=root, check=True,
        )
    raise AssertionError("unreachable comparison range")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("base", nargs="?", default="")
    parser.add_argument("before", nargs="?", default="")
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    args = parser.parse_args(argv)
    try:
        print(comparison_range(args.root.resolve(), args.base, args.before))
    except (OSError, subprocess.CalledProcessError) as error:
        print(f"comparison range unavailable: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
