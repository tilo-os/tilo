#!/usr/bin/env python3
"""Land the commits checked out here on main: rebase onto main, push, and again while main moves.

    tools/land.py                  rebase HEAD onto origin's main and push it there
    tools/land.py --attempts 20    allow more rounds while main keeps moving

Everyone pushes straight to main, and the push hook lints what a push changes before its refs
leave (tools/ci/pre_push_lint.py). When main moves while the lint runs, the remote refuses the
push for its old value, whatever the lint said. Each round here fetches main, rebases onto it and
pushes again at once. The hook remembers a pass by the push's own patch, so a rebase that leaves
the pushed change as it was is sent on that pass in seconds; one onto commits that changed the
same files is linted again, and t answers each check whose reads main left alone from its cache.

A round ends the landing when the push lands, when the hook refuses the change itself (its lint
failed: fix it and land again), when the rebase meets a conflict (the rebase is aborted, and the
branch is left as it was), or when the push fails for anything but main having moved. The pushed
commits are those checked out, so a change on disk that is not committed is refused first: the
lint reads the files on disk, and a pass of files the push does not send speaks for nothing.
"""
from __future__ import annotations

import argparse
from pathlib import Path
import subprocess
import sys
import time

TOOLS = Path(__file__).resolve().parent
if str(TOOLS) not in sys.path:
    sys.path.append(str(TOOLS))
from ci.pre_push_lint import worktree_differs  # noqa: E402

MAIN = "main"
# What the push hook writes when it refuses the change itself: another round would refuse it
# again, after linting it again.
REFUSED = "pre-push: refusing"


def say(message: str) -> None:
    print(f"land: {message}", file=sys.stderr, flush=True)


def git(root: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess:
    return subprocess.run(["git", "-C", str(root), *args], capture_output=True, text=True,
                          check=check)


def fetch_main(root: Path, remote: str) -> str:
    git(root, "fetch", "--quiet", remote, MAIN)
    return git(root, "rev-parse", "FETCH_HEAD").stdout.strip()


def contains(root: Path, commit: str) -> bool:
    return git(root, "merge-base", "--is-ancestor", commit, "HEAD", check=False).returncode == 0


def rebase(root: Path, main: str) -> bool:
    """Rebase HEAD onto main; on a conflict, abort it and leave the branch as it was."""
    result = git(root, "rebase", "--quiet", main, check=False)
    if result.returncode == 0:
        return True
    for line in (result.stdout + result.stderr).strip().splitlines():
        say(line)
    git(root, "rebase", "--abort", check=False)
    return False


def push(root: Path, remote: str) -> tuple[int, str]:
    """Push HEAD to main, showing what Git and the hook say as they say it."""
    process = subprocess.Popen(["git", "-C", str(root), "push", remote, f"HEAD:{MAIN}"],
                               stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    said = []
    assert process.stdout is not None
    for line in process.stdout:
        sys.stderr.write(line)
        sys.stderr.flush()
        said.append(line)
    return process.wait(), "".join(said)


def land(root: Path, remote: str, attempts: int) -> int:
    if git(root, "status", "--porcelain", "--untracked-files=no").stdout.strip():
        say("the checkout has changes that are not committed; commit or drop them first: the "
            "rebase needs a clean tree, and the lint reads the files on disk")
        return 1
    if worktree_differs(root):
        say("there are untracked files in the checkout that a package may read; the lint would "
            "read them and its pass would speak for files the push does not send. Commit, move "
            "or remove them first")
        return 1
    started = time.monotonic()
    for attempt in range(1, attempts + 1):
        main = fetch_main(root, remote)
        if not contains(root, main):
            say(f"rebasing onto main at {main[:12]}")
            if not rebase(root, main):
                say("the rebase met a conflict and was aborted; resolve it with "
                    f"`git rebase {remote}/{MAIN}` and land again")
                return 1
        if git(root, "rev-parse", "HEAD").stdout.strip() == main:
            say("main already holds everything checked out here; nothing to land")
            return 0
        status, output = push(root, remote)
        if status == 0:
            head = git(root, "rev-parse", "HEAD").stdout.strip()
            say(f"landed {head[:12]} on main in {time.monotonic() - started:.0f}s "
                f"(attempt {attempt})")
            return 0
        if REFUSED in output:
            say("the push hook refused the change (above); fix it and land again")
            return status
        if contains(root, fetch_main(root, remote)):
            say("the push failed (above), and not because main moved; not retrying")
            return status
        say(f"main moved while the push ran (attempt {attempt} of {attempts}); "
            "rebasing and pushing again")
    say(f"main moved under each of {attempts} attempts; run tools/land.py again")
    return 1


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--remote", default="origin", help="the remote whose main to land on")
    parser.add_argument("--attempts", type=int, default=10,
                        help="how many rounds of rebase and push to make while main moves")
    options = parser.parse_args(argv)
    if options.attempts < 1:
        parser.error("--attempts takes a positive number")
    root = Path(git(Path.cwd(), "rev-parse", "--show-toplevel").stdout.strip())
    return land(root, options.remote, options.attempts)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
