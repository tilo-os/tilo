#!/usr/bin/env python3
"""Keep the repository self-contained: no pointers to private trackers or documents.

This is an open-source repository. Code, comments, docs, tests, scripts and
commit messages must state a reason where it is needed instead of pointing at an
issue-tracker id or an internal link a reader cannot open. Only vendor/ (upstream
code, carried as it ships) is exempt; there is no other allow-list.

  check_self_contained.py            scan every tracked file at the working tree
  check_self_contained.py --staged   scan the lines the index adds (pre-commit)

Commit messages go through the same pattern in check_commit_message.py.
The patterns are spelled with character classes so this file does not match itself.
"""
import argparse
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
EXEMPT = ("vendor/",)

# An internal issue id (any case, not inside a longer word) and the hosts of
# private trackers, docs and chat threads.
REFERENCE = re.compile(
    r"(?<![A-Za-z0-9_])t[i]l-[0-9]+"
    r"|(?<![A-Za-z0-9_])(?:linear[.]app|notion[.](?:so|site)|app[.]slack[.]com/client)(?![A-Za-z0-9_])"
    r"|slack[.]com/archives",
    re.IGNORECASE,
)
# The same pattern in POSIX ERE for `git grep`.
GIT_GREP = (
    r"(^|[^[:alnum:]_])t[i]l-[0-9]+"
    r"|(^|[^[:alnum:]_])(linear[.]app|notion[.](so|site)|app[.]slack[.]com/client)($|[^[:alnum:]_])"
    r"|slack[.]com/archives"
)
ADVICE = "State the reason itself instead of pointing at an internal tracker, document or chat link."


def find_reference(text):
    """The first internal reference in `text`, or None."""
    match = REFERENCE.search(text)
    return match[0] if match else None


def git(root, *args):
    return subprocess.run(["git", "-C", str(root), *args], capture_output=True, check=False)


def tree_hits(root):
    result = git(root, "grep", "-I", "-n", "-i", "-E", "-e", GIT_GREP, "--", ".", ":!vendor")
    if result.returncode not in (0, 1):
        raise RuntimeError("git grep failed: " + result.stderr.decode(errors="replace"))
    for line in result.stdout.decode(errors="replace").splitlines():
        path, _, rest = line.partition(":")
        if not path.startswith(EXEMPT):
            yield path + ":" + rest[:160]


def staged_hits(root):
    diff = git(root, "diff", "--cached", "-U0", "--no-renames", "--diff-filter=ACM", "--", ".", ":!vendor")
    if diff.returncode:
        raise RuntimeError("git diff failed: " + diff.stderr.decode(errors="replace"))
    path = ""
    number = 0
    for line in diff.stdout.decode(errors="replace").splitlines():
        if line.startswith("+++ b/"):
            path = line[6:]
        elif line.startswith("@@"):
            number = int(re.search(r"\+(\d+)", line)[1]) - 1
        elif line.startswith("+") and not line.startswith("+++"):
            number += 1
            if find_reference(line) and not path.startswith(EXEMPT):
                yield f"{path}:{number}:{line[1:161]}"


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--staged", action="store_true")
    args = parser.parse_args()
    try:
        hits = list((staged_hits if args.staged else tree_hits)(ROOT))
    except RuntimeError as error:
        print("self-contained check failed: " + str(error), file=sys.stderr)
        return 1
    if hits:
        print("\n".join(hits[:50]), file=sys.stderr)
        more = f" (and {len(hits) - 50} more)" if len(hits) > 50 else ""
        print(f"Internal references found{more}. {ADVICE}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
