#!/usr/bin/env python3
"""Validate Conventional Commits and Git-generated merge/revert/autosquash messages.

--title checks a pull request title, which becomes the subject of its merge
commit, for internal references only.
"""
import argparse
from pathlib import Path
import re
import subprocess
import sys

from check_public_references import internal_reference

TYPES = frozenset("build chore ci docs dev env feat fix perf refactor revert style test".split())
HEADER = re.compile(r"([a-z][a-z0-9-]*)(?:\(([^()\r\n]+)\))?(!)?: (\S.*)")
MERGE = re.compile(r"Merge (?:branch|remote-tracking branch|tag|pull request|commit|main) .+")
EXAMPLES = "Examples:\n  fix(shell): preserve window identity\n  feat(ui)!: expose resource actions\n\n  docs: explain hook installation\n\n  A multiline body can follow a blank line.\n  BREAKING CHANGE: describe the incompatible behavior here."


def git(root, *args):
    return subprocess.check_output(["git", "-C", str(root), *args]).decode().strip()


def public_reference_failure(text):
    # Only authored text: generated merge, revert and autosquash subjects repeat
    # titles and branch names from history that predates this rule.
    hit = internal_reference(text)
    if hit:
        return ("'" + hit + "' points at the internal issue tracker, which readers of this public"
                " repository cannot open; say what changed and why instead.")
    return None


def validate(message, root, merge=False, parent=None):
    lines = message.splitlines()
    if not lines or not lines[0].strip():
        return "The subject is empty."
    subject = lines[0]
    if "\r" in message or "\x00" in message:
        return "Commit messages must be text with LF line endings."
    match = HEADER.fullmatch(subject)
    if match:
        kind, scope, _, _ = match.groups()
        if kind not in TYPES:
            return "Unknown type; use one of: " + ", ".join(sorted(TYPES)) + "."
        if scope is not None and (not scope.strip() or scope != scope.strip()):
            return "Use a nonempty scope without leading or trailing spaces."
        if len(lines) > 1 and lines[1].strip():
            return "Separate the subject and multiline body with a blank line."
        return public_reference_failure(message)
    if merge and MERGE.fullmatch(subject):
        return None
    if subject.startswith('Revert "') and subject.endswith('"'):
        target = re.search(r"^This reverts commit ([0-9a-f]{40,64})(?:\.|,)", message, re.M)
        if target:
            try:
                original = git(root, "show", "-s", "--format=%s", target[1])
            except subprocess.CalledProcessError:
                return "The generated revert names a commit that cannot be inspected."
            if subject == 'Revert "' + original + '"':
                return None
        return "A generated revert must retain Git's exact subject and target-commit footer."
    for prefix in ("fixup! ", "squash! ", "amend! "):
        if subject.startswith(prefix):
            target = subject[len(prefix):]
            if not HEADER.fullmatch(target):
                return "Autosquash must target a Conventional Commit subject."
            try:
                subjects = git(root, "log", "--format=%s", parent or "HEAD").splitlines()
            except subprocess.CalledProcessError:
                return "The autosquash target history cannot be inspected."
            if target in subjects and validate(target, root) is None:
                return None
            return "The autosquash target must be an existing Conventional Commit in this history."
    return "Use type(optional-scope)!: description; ':' must be followed by one space."


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    target = parser.add_mutually_exclusive_group(required=True)
    target.add_argument("message", type=Path, nargs="?")
    target.add_argument("--title", help="a pull request title")
    args = parser.parse_args()
    if args.title is not None:
        failure = public_reference_failure(args.title)
        if failure:
            print("Pull request title rejected: " + failure, file=sys.stderr)
            return 1
        return 0
    root = Path(git(Path.cwd(), "rev-parse", "--show-toplevel"))
    merge_path = Path(git(root, "rev-parse", "--git-path", "MERGE_HEAD"))
    if not merge_path.is_absolute():
        merge_path = root / merge_path
    try:
        message = args.message.read_bytes().decode("utf-8")
        failure = validate(message, root, merge=merge_path.is_file())
    except (UnicodeError, OSError, subprocess.CalledProcessError) as error:
        failure = "Cannot inspect the commit message: " + str(error)
    if failure:
        print("Commit rejected: " + failure + "\n" + EXAMPLES, file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
