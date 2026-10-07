#!/usr/bin/env python3
"""CI counterpart of commit-msg and pre-commit, including commits made with --no-verify."""
import argparse
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools/ci"))
from check_changed_rust_allows import comparison_range
from check_commit_message import EXAMPLES, validate
from check_source_content import content_issues


class IncompleteHistory(ValueError):
    pass


def git(root, *args):
    return subprocess.check_output(["git", "-C", str(root), *args])


def inspect(root, base, head="HEAD"):
    baseline = {}
    for record in git(root, "ls-tree", "-r", "-z", base).split(b"\0"):
        if record:
            metadata, path = record.split(b"\t", 1)
            baseline[path] = metadata.split()[2]
    ancestors = set(git(root, "rev-list", base).decode().splitlines())
    commits = git(root, "rev-list", "--reverse", base + ".." + head).decode().splitlines()
    shallow_path = Path(git(root, "rev-parse", "--git-path", "shallow").decode().strip())
    if not shallow_path.is_absolute():
        shallow_path = root / shallow_path
    shallow = set(shallow_path.read_text().splitlines()) if shallow_path.is_file() else set()
    failures = []
    for commit in commits:
        raw = git(root, "cat-file", "commit", commit)
        headers, message = raw.split(b"\n\n", 1)
        parents = [line[7:].decode() for line in headers.splitlines() if line.startswith(b"parent ")]
        if commit in shallow and any(parent not in ancestors for parent in parents):
            raise IncompleteHistory("Incomplete new history at " + commit + "; deepen both comparison endpoints before checking.")
        for parent in parents:
            result = subprocess.run(["git", "-C", str(root), "cat-file", "-e", parent + "^{commit}"], capture_output=True)
            if result.returncode:
                raise IncompleteHistory("Incomplete history at " + commit + "; fetch/deepen both comparison endpoints before checking.")
        try:
            failure = validate(message.decode("utf-8"), root, merge=len(parents) > 1, parent=parents[0] if parents else None)
        except UnicodeError:
            failure = "Commit message is not UTF-8 text."
        if failure:
            failures.append(commit[:12] + ": " + failure)
        args = ("diff-tree", "-r", "--raw", "--no-abbrev", "-z", "--no-renames", "--no-commit-id")
        changes = git(root, *args, parents[0], commit) if parents else git(root, *args, "--root", commit)
        records = changes.split(b"\0")
        for index in range(0, len(records) - 1, 2):
            metadata, path = records[index], records[index + 1]
            if not metadata:
                continue
            fields = metadata.split()
            mode, oid = fields[1], fields[3]
            if mode == b"000000":
                continue
            if mode == b"160000":
                failures.append(commit[:12] + ": uninspected submodule " + path.decode(errors="replace"))
                continue
            issues = content_issues(git(root, "cat-file", "blob", oid.decode()))
            if issues and baseline.get(path) != oid:
                failures.append(commit[:12] + ": " + path.decode(errors="replace") + ": " + ", ".join(issues) + "; new/modified non-text or non-LF blobs are forbidden")
    return failures, len(commits)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("base", nargs="?", default="")
    parser.add_argument("before", nargs="?", default="")
    args = parser.parse_args()
    try:
        interval = comparison_range(ROOT, args.base, args.before)
        base = interval.split("..", 1)[0]
        for depth in (64, 256, 1024, 4096, None):
            try:
                failures, count = inspect(ROOT, base)
                break
            except IncompleteHistory as error:
                if depth is None or git(ROOT, "rev-parse", "--is-shallow-repository").strip() != b"true":
                    raise
                raw = git(ROOT, "cat-file", "commit", "HEAD").split(b"\n\n", 1)[0]
                endpoints = [line[7:].decode() for line in raw.splitlines() if line.startswith(b"parent ")]
                result = subprocess.run(["git", "-C", str(ROOT), "fetch", "--no-tags", "--deepen=" + str(depth), "origin", base, *endpoints], capture_output=True)
                if result.returncode:
                    raise IncompleteHistory(str(error) + " Automatic deepening failed; fetch the missing comparison history explicitly.") from error
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        print("Commit policy inspection failed: " + str(error), file=sys.stderr)
        return 1
    if failures:
        print("\n".join(failures) + "\n" + EXAMPLES, file=sys.stderr)
        return 1
    print(f"Commit policy OK: {count} new commits inspected, including exact changed blobs")
    return 0


if __name__ == "__main__":
    sys.exit(main())
