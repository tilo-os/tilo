#!/usr/bin/env python3
"""Fetch only enough immutable PR history to prove the merge base."""
import json
import os
from pathlib import Path
import re
import subprocess
import sys


DEPTHS = (32, 128, 512, 2048)
FULL_SHA = re.compile(r"[0-9a-f]{40}|[0-9a-f]{64}")


def git(repo, *args, check=True):
    return subprocess.run(
        ["git", "-C", str(repo), *args],
        check=check,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )


def merge_base(repo, base, head):
    result = git(repo, "merge-base", base, head, check=False)
    return result.stdout.strip() if result.returncode == 0 else None


def prepare(repo, event, depths=DEPTHS):
    refs = [event["pull_request"][side]["sha"] for side in ("base", "head")]
    for ref in refs:
        if not isinstance(ref, str) or not FULL_SHA.fullmatch(ref):
            raise ValueError("PR comparison requires immutable full commit IDs")
    base, head = refs
    if git(repo, "rev-parse", "HEAD").stdout.strip() != head:
        raise ValueError("checked out revision does not match the PR head")

    ancestor = merge_base(repo, base, head)
    if ancestor:
        return ancestor

    for depth in depths:
        result = git(
            repo, "fetch", "--no-tags", "--no-write-fetch-head", f"--depth={depth}",
            "origin", f"+{base}:refs/ci/pr-base", f"+{head}:refs/ci/pr-head",
            check=False,
        )
        if result.returncode != 0:
            raise RuntimeError(f"targeted PR history fetch failed: {result.stderr.strip()}")
        ancestor = merge_base(repo, base, head)
        if ancestor:
            return ancestor
    raise RuntimeError(f"cannot establish PR merge base after depth {depths[-1]}")


def main():
    try:
        event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text(encoding="utf-8"))
        ancestor = prepare(Path.cwd(), event)
        print(f"PR merge base ready: {ancestor}")
    except (OSError, KeyError, TypeError, ValueError, RuntimeError,
            subprocess.CalledProcessError, json.JSONDecodeError) as error:
        print(f"Cannot prepare PR history: {type(error).__name__}: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
