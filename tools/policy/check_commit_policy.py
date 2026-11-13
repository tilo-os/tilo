#!/usr/bin/env python3
"""CI counterpart of commit-msg and pre-commit, including commits made with --no-verify.

Every new commit's message, its exact changed blobs (text, LF), the lines it
adds (credentials, merge-conflict markers, dbg!: tools/policy/added_line_policy.py),
its changed wire enums (a variant a parent had, kept in its place:
tools/policy/check_wire_enums.py) and its allowance markers (one goes only with
the code it covers: tools/policy/check_allowance_markers.py).
"""
import argparse
import os
from contextlib import contextmanager
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools/ci"))
sys.path.insert(0, str(ROOT / "tools/policy"))
from comparison_range import comparison_range
from check_commit_message import EXAMPLES, validate
from check_public_references import internal_reference
from check_source_content import content_issues
from check_allowance_markers import (
    declared_keys,
    diff_problems as marker_problems,
    in_scope as marker_scope,
    read_families,
)
from check_wire_enums import allowed_breaks, diff_problems, in_scope, wire_enums
from added_line_policy import GUIDANCE, added_line_issues, added_lines

BREAKS = "tools/policy/wire_enum_breaks.toml"

DIFF_TREE = ("diff-tree", "-r", "--raw", "--no-abbrev", "-z", "--no-renames", "--no-commit-id")
# Modes whose object is not a blob: absent, and a submodule's commit.
NOT_A_BLOB = (b"000000", b"160000")


class IncompleteHistory(ValueError):
    pass


def git(root, *args):
    return subprocess.check_output(["git", "-C", str(root), *args])


@contextmanager
def blob_reader(root):
    """Read every exact changed blob through one persistent Git process."""
    with subprocess.Popen(["git", "-C", str(root), "cat-file", "--batch"],
                          stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                          stderr=subprocess.DEVNULL) as process:
        def read(oid):
            process.stdin.write(oid + b"\n")
            process.stdin.flush()
            header = process.stdout.readline().split()
            if len(header) != 3 or header[:2] != [oid, b"blob"]:
                raise ValueError("Cannot inspect exact changed blob.")
            size = int(header[2])
            if size < 0:
                raise ValueError("Invalid changed blob size.")
            data = process.stdout.read(size)
            if len(data) != size or process.stdout.read(1) != b"\n":
                raise ValueError("Incomplete changed blob.")
            return data
        try:
            yield read
        finally:
            process.stdin.close()
            process.stdout.close()
    if process.returncode:
        raise ValueError("Git changed blob reader failed.")


def changed_blobs(root, parent, commit):
    """{path: (old mode, new mode, old oid, new oid)} for what `commit` changes."""
    changes = git(root, *DIFF_TREE, parent, commit) if parent else git(root, *DIFF_TREE, "--root", commit)
    records = changes.split(b"\0")
    result = {}
    for index in range(0, len(records) - 1, 2):
        metadata, path = records[index], records[index + 1]
        if metadata:
            fields = metadata.split()
            result[path] = (fields[0].lstrip(b":"), fields[1], fields[2], fields[3])
    return result


def parent_versions(path, change, other_parents):
    """Oids of every parent's version of `path`, or None when one parent already has the result.

    A merge is charged only with paths its resolution changed relative to every
    parent; a file taken verbatim from either side passed that side's checks.
    """
    versions = [change[2]] if change[0] not in NOT_A_BLOB else []
    for other in other_parents:
        if path not in other:
            return None
        if other[path][0] not in NOT_A_BLOB:
            versions.append(other[path][2])
    return versions


def wire_breaks(root, commit):
    """The (file, enum) pairs `commit` names as broken on purpose."""
    listed = subprocess.run(["git", "-C", str(root), "show", commit + ":" + BREAKS],
                            capture_output=True)
    return allowed_breaks(listed.stdout.decode()) if not listed.returncode else set()


def tree_blobs(root, commit):
    """{path: blob oid} for every file `commit`'s tree holds."""
    found = {}
    for record in git(root, "ls-tree", "-r", "-z", commit).split(b"\0"):
        if record:
            metadata, path = record.split(b"\t", 1)
            mode, _, oid = metadata.split()
            if mode not in NOT_A_BLOB:
                found[path] = oid
    return found


def wire_failures(root, commit, touched, parents_changes, wire_now, read_blob):
    """What `commit`'s enums break of every parent's version of the files it touches."""
    allowed = wire_breaks(root, commit)
    problems = []
    for path in sorted(touched):
        name = path.decode(errors="replace")
        if not in_scope(name):
            continue
        for changes in parents_changes:
            change = changes.get(path)
            if change is None or change[0] in NOT_A_BLOB:
                continue
            before = wire_enums(read_blob(change[2]).decode(errors="replace"))
            problems.extend(commit[:12] + ": " + problem
                            for problem in diff_problems({name: before}, wire_now, allowed))
    return problems


def marker_failures(commit, touched, parents_changes, tree, declared, families, read_blob):
    """What `commit`'s allowance markers lose of every parent's version of the files it touches."""
    problems = []
    for path in sorted(touched):
        name = path.decode(errors="replace")
        if not marker_scope(name):
            continue
        current = tree.get(path)
        after = read_blob(current).decode(errors="replace") if current else ""
        for changes in parents_changes:
            change = changes.get(path)
            if change is None or change[0] in NOT_A_BLOB:
                continue
            before = read_blob(change[2]).decode(errors="replace")
            problems.extend(commit[:12] + ": " + problem
                            for problem in marker_problems(name, before, after, declared, families))
    return problems


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
    added_line_hit = False
    # Which patterns the audits declare now: a marker naming none is dead, and
    # removing a dead marker takes no allowance away. A marker family no scan of
    # the tree reads any more is dead for the same reason.
    declared = declared_keys(root)
    families = read_families(root)
    with blob_reader(root) as read_blob:
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
            other_parents = [changed_blobs(root, parent, commit) for parent in parents[1:]]
            changes = changed_blobs(root, parents[0] if parents else None, commit)
            parents_changes = [changes] + other_parents if parents else []
            if parents_changes:
                touched = set().union(*parents_changes)
                tree = tree_blobs(root, commit)
                wire_now = {}
                for path in touched:
                    name = path.decode(errors="replace")
                    if in_scope(name) and path in tree:
                        wire_now[name] = wire_enums(read_blob(tree[path]).decode(errors="replace"))
                failures.extend(wire_failures(root, commit, touched, parents_changes, wire_now, read_blob))
                failures.extend(marker_failures(commit, touched, parents_changes, tree, declared, families, read_blob))
            for path, change in changes.items():
                mode, oid = change[1], change[3]
                if mode == b"000000":
                    continue
                if mode == b"160000":
                    failures.append(commit[:12] + ": uninspected submodule " + path.decode(errors="replace"))
                    continue
                data = read_blob(oid)
                issues = content_issues(data)
                if issues and baseline.get(path) != oid:
                    failures.append(commit[:12] + ": " + path.decode(errors="replace") + ": " + ", ".join(issues) + "; new/modified non-text or non-LF blobs are forbidden")
                versions = None if issues else parent_versions(path, change, other_parents)
                if versions is not None:
                    lines = added_lines(data, [read_blob(version) for version in versions])
                    for finding in added_line_issues(path.decode(errors="replace"), lines):
                        added_line_hit = True
                        failures.append(commit[:12] + ": " + finding[:160])
    if added_line_hit:
        failures.append(GUIDANCE)
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
    # The pull request's head branch name, when CI provides it (PR_HEAD_REF):
    # it lands in the merge commit subject on main.
    head_ref = os.environ.get("PR_HEAD_REF", "")
    reference = internal_reference(head_ref)
    if reference:
        failures.append(f"branch name {head_ref!r}: '{reference}' is an internal reference;"
                        " rename the branch after what it changes.")
    if failures:
        print("\n".join(failures) + "\n" + EXAMPLES, file=sys.stderr)
        return 1
    print(f"Commit policy OK: {count} new commits inspected, including exact changed blobs")
    return 0


if __name__ == "__main__":
    sys.exit(main())
