#!/usr/bin/env python3
"""Fetch only the immutable Git history required by app releases."""

from __future__ import annotations

import argparse
from pathlib import Path
import re
import subprocess
import sys


DEPTHS = (32, 128, 512, 2048)
FULL_SHA = re.compile(r"(?:[0-9a-f]{40}|[0-9a-f]{64})")
BASE_REF = "refs/ci/app-release-base"
MAIN_REF = "refs/ci/app-release-main"


def git(repo: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", "-C", str(repo), *args],
        check=check,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )


def full_sha(value: str, label: str) -> str:
    if not FULL_SHA.fullmatch(value):
        raise ValueError(f"{label} must be an immutable full commit ID")
    return value


def fetch_exact(repo: Path, sha: str, destination: str, depth: int = 1) -> None:
    full_sha(sha, "release comparison")
    result = git(
        repo,
        "fetch",
        "--no-tags",
        "--no-write-fetch-head",
        f"--depth={depth}",
        "origin",
        f"+{sha}:{destination}",
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError(f"targeted release history fetch failed: {result.stderr.strip()}")
    fetched = git(repo, "rev-parse", destination).stdout.strip()
    if fetched != sha:
        raise RuntimeError(f"targeted release history resolved {fetched}, expected {sha}")
    git(repo, "cat-file", "-e", f"{sha}^{{commit}}")


def remote_main(repo: Path) -> str:
    result = git(
        repo,
        "ls-remote",
        "--exit-code",
        "--refs",
        "origin",
        "refs/heads/main",
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError(f"cannot resolve remote main: {result.stderr.strip()}")
    rows = [line.split() for line in result.stdout.splitlines() if line.strip()]
    if len(rows) != 1 or len(rows[0]) != 2 or rows[0][1] != "refs/heads/main":
        raise RuntimeError("remote main did not resolve to exactly one branch tip")
    return full_sha(rows[0][0], "remote main")


def is_ancestor(repo: Path, ancestor: str, descendant: str) -> bool:
    result = git(repo, "merge-base", "--is-ancestor", ancestor, descendant, check=False)
    if result.returncode not in (0, 1):
        raise RuntimeError(f"cannot inspect release ancestry: {result.stderr.strip()}")
    return result.returncode == 0


def main_has_shallow_boundary(repo: Path, main_sha: str) -> bool:
    shallow_path = Path(git(repo, "rev-parse", "--git-path", "shallow").stdout.strip())
    if not shallow_path.is_absolute():
        shallow_path = repo / shallow_path
    try:
        boundaries = shallow_path.read_text(encoding="ascii").splitlines()
    except FileNotFoundError:
        return False
    return any(is_ancestor(repo, boundary, main_sha) for boundary in boundaries)


def require_main_ancestor(
    repo: Path, candidate: str, depths: tuple[int, ...] = DEPTHS
) -> str:
    candidate = full_sha(candidate, "release source")
    checked_out = git(repo, "rev-parse", "HEAD").stdout.strip()
    if checked_out != candidate:
        raise ValueError(
            f"checked out revision {checked_out} does not match release source {candidate}"
        )

    # Observe main once, then deepen that immutable tip. A moving branch cannot
    # change the ancestry decision halfway through the release plan.
    main_sha = remote_main(repo)
    fetch_exact(repo, main_sha, MAIN_REF)
    for depth in depths:
        if is_ancestor(repo, candidate, main_sha):
            return main_sha
        if not main_has_shallow_boundary(repo, main_sha):
            raise RuntimeError(
                f"release source {candidate} is not an ancestor of main {main_sha}"
            )
        fetch_exact(repo, main_sha, MAIN_REF, depth)

    if is_ancestor(repo, candidate, main_sha):
        return main_sha
    if main_has_shallow_boundary(repo, main_sha):
        # Use an effectively unbounded depth, then verify that the selected
        # main graph has no shallow boundary before making a negative decision.
        fetch_exact(repo, main_sha, MAIN_REF, 2_147_483_647)
    if is_ancestor(repo, candidate, main_sha):
        return main_sha
    if main_has_shallow_boundary(repo, main_sha):
        raise RuntimeError("cannot establish complete remote main ancestry")
    raise RuntimeError(
        f"release source {candidate} is not an ancestor of main {main_sha}"
    )


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    subcommands = result.add_subparsers(dest="command", required=True)
    fetch = subcommands.add_parser("fetch-base")
    fetch.add_argument("sha")
    ancestor = subcommands.add_parser("require-main-ancestor")
    ancestor.add_argument("sha")
    return result


def main() -> int:
    args = parser().parse_args()
    try:
        if args.command == "fetch-base":
            fetch_exact(Path.cwd(), args.sha, BASE_REF)
            print(f"release comparison ready: {args.sha}")
        else:
            observed = require_main_ancestor(Path.cwd(), args.sha)
            print(f"release source is on observed main: {observed}")
    except (OSError, ValueError, RuntimeError, subprocess.CalledProcessError) as error:
        print(
            f"Cannot prepare app release history: {type(error).__name__}: {error}",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
