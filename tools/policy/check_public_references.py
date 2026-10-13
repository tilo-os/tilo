#!/usr/bin/env python3
"""Keep tracked files free of references to the internal issue tracker and private documents.

The repository is public, so its text has to stand on its own: an issue key or a
tracker link tells an outside reader nothing. Say what changed and why instead;
public pull requests (#1234) and commits are fine to cite.

The default mode reads every tracked file in the working tree; --staged reads
the staged blobs of the files a commit adds or changes.
"""
import argparse
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
# Issue keys, also inside identifiers and labels, tracker links, prose that
# points at tracker documents, and links to private documents and chat threads.
# git grep preselects lines cheaply; PATTERN decides.
GIT_PATTERNS = (
    r"til-?[0-9]", r"linear\.app/", r"linear (document|issue|ticket|project|note)",
    r"notion\.(so|site)/", r"slack\.com/(archives|client)/",
)
PATTERN = re.compile(
    r"(?<![^\W_])til-?\d+(?![^\W_])|linear\.app/|\bLinear (?:document|issue|ticket|project|note)s?\b"
    r"|(?<![^\W_])notion\.(?:so|site)/|slack\.com/(?:archives|client)/",
    re.IGNORECASE,
)


def internal_reference(text):
    """The first internal tracker reference in `text`, or None."""
    match = PATTERN.search(text)
    return match.group(0) if match else None


def audit(root, staged=False):
    args = ["git", "-C", str(root), "grep", "-n", "-I", "-i", "-E"]
    for pattern in GIT_PATTERNS:
        args += ["-e", pattern]
    if staged:
        changed = subprocess.run(
            ["git", "-C", str(root), "diff", "--cached", "--name-only", "-z", "--diff-filter=ACMR"],
            capture_output=True, check=True,
        ).stdout.decode().split("\0")
        changed = [path for path in changed if path]
        if not changed:
            return []
        args += ["--cached", "--", *changed]
    result = subprocess.run(args, capture_output=True, text=True)
    if result.returncode not in (0, 1):
        raise ValueError(result.stderr.strip() or "git grep failed")
    failures = []
    for line in result.stdout.splitlines():
        path, number, text = line.split(":", 2)
        hit = internal_reference(text)
        if hit:
            failures.append(f"{path}:{number}: {hit}")
    return failures


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--staged", action="store_true")
    args = parser.parse_args()
    try:
        failures = audit(ROOT, args.staged)
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        print("Public reference inspection failed: " + str(error), file=sys.stderr)
        return 1
    if failures:
        print("\n".join(failures), file=sys.stderr)
        print(
            "\nThe repository is public: replace each internal tracker reference with what the reader"
            " needs, the behaviour or the reason in plain words. Citing a public pull request (#1234)"
            " or commit is fine.",
            file=sys.stderr,
        )
        return 1
    print("Public references OK: no internal tracker references in tracked files")
    return 0


if __name__ == "__main__":
    sys.exit(main())
