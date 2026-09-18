#!/usr/bin/env python3
"""Find a checked ancestor so coalesced main pushes include cancelled changes.

Receipts are coverage hints, not merge authorization. Missing or unreadable
evidence selects a full check. Only strict successful receipts on the first-parent
history qualify; the current commit never qualifies, including on reruns.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import subprocess

NOTES = "refs/notes/tilo-check-ci"


def git(repo: Path, *args: str) -> str:
    return subprocess.check_output(
        ["git", "-C", str(repo), *args], text=True, encoding="utf-8",
        stderr=subprocess.PIPE, timeout=60,
    ).rstrip("\n")


def select_base(repo: Path, head: str = "HEAD", fetch: str | None = None,
                limit: int = 1000) -> dict:
    def full(reason):
        return {"base": "", "full": True, "reason": reason}

    if limit < 1:
        raise ValueError("history limit must be positive")
    try:
        if fetch is not None:
            # Force replaces stale local notes with the current remote snapshot.
            git(repo, "fetch", "--no-tags", fetch, f"+{NOTES}:{NOTES}")
        commit = git(repo, "rev-parse", "--verify", head + "^{commit}")
        history = git(repo, "rev-list", "--first-parent", f"--max-count={limit + 1}", commit).splitlines()[1:]
        notes = dict(line.split()[::-1] for line in git(repo, "notes", f"--ref={NOTES}", "list").splitlines())
        for ancestor in history:
            blob = notes.get(ancestor)
            if blob is None:
                continue
            receipt = git(repo, "cat-file", "blob", blob)
            expected = rf"select=success security=success affected=success run=[0-9]+ sha={ancestor}"
            if re.fullmatch(expected, receipt):
                return {"base": ancestor, "full": False, "reason": "successful-first-parent-receipt"}
        return full("no-successful-first-parent-receipt")
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired, OSError, ValueError, UnicodeError):
        return full("receipt-or-history-unavailable")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=Path.cwd())
    parser.add_argument("--head", default="HEAD")
    parser.add_argument("--fetch", metavar="REMOTE", help="Refresh receipts; fetch failure selects full coverage")
    parser.add_argument("--limit", type=int, default=1000)
    args = parser.parse_args()
    print(json.dumps(select_base(args.repo, args.head, args.fetch, args.limit)))


if __name__ == "__main__":
    main()
