#!/usr/bin/env python3
"""Check actual Git blobs for UTF-8 text and LF, with exact asset exceptions.

The index mode inspects the bytes being committed, including sparse checkouts.
Exceptions preserve required assets and checksum-pinned upstream bytes; changing
an exception requires an explicit policy edit. File extensions grant no exemption.
"""
import argparse
import json
from pathlib import Path
import re
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[2]
EXCEPTIONS = "tools/policy/source_content_exceptions.json"
NON_TEXT_BYTES = re.compile(rb"[\x00-\x08\x0e-\x1f]")


def git(root, *args):
    return subprocess.check_output(["git", "-C", str(root), *args])


def committed_tree(root):
    """HEAD's tree, or the index's in a repository without commits.

    t checks run on a copy of the tree without .git, under
    tools/ci/t-git-index.sh, whose throwaway repository indexes exactly the
    files present and commits nothing: its index is the tree to inspect.
    """
    try:
        return git(root, "rev-parse", "--verify", "--quiet", "HEAD^{tree}").decode().strip()
    except subprocess.CalledProcessError as tree_error:
        if tree_error.returncode != 1:
            raise
        # Only an absent HEAD admits the index fallback. A missing tree in an
        # existing commit must not turn a committed-source audit into an index audit.
        try:
            git(root, "rev-parse", "--verify", "--quiet", "HEAD")
        except subprocess.CalledProcessError as head_error:
            if head_error.returncode != 1:
                raise
            return git(root, "write-tree").decode().strip()
        raise tree_error


def content_issues(data):
    try:
        data.decode("utf-8")
    except UnicodeDecodeError:
        return ["non-text content"]
    if NON_TEXT_BYTES.search(data):
        return ["non-text content"]
    return ["non-LF line endings"] if b"\r" in data else []


def audit(root, staged=False):
    if staged:
        records = git(root, "ls-files", "--stage", "-z").split(b"\0")
        manifest = git(root, "show", ":" + EXCEPTIONS)
    else:
        tree = committed_tree(root)
        records = git(root, "ls-tree", "-r", "-z", tree).split(b"\0")
        manifest = git(root, "show", tree + ":" + EXCEPTIONS)
    exceptions = json.loads(manifest)
    allowed = {}
    for entry in exceptions:
        key = entry["path"]
        if key in allowed or not entry["reason"].strip():
            raise ValueError("duplicate or unexplained asset exception: " + key)
        allowed[key] = entry["oid"]
    entries = []
    for record in records:
        if not record:
            continue
        metadata, path = record.split(b"\t", 1)
        mode, second, third = metadata.decode().split()
        if staged and third != "0":
            raise ValueError("unmerged index entry: " + path.decode())
        if mode == "160000":
            raise ValueError("uninspected submodule: " + path.decode())
        entries.append((second if staged else third, path.decode()))
    previous = {}
    if staged:
        head = subprocess.run(["git", "-C", str(root), "rev-parse", "--verify", "--quiet", "HEAD"], capture_output=True)
        if head.returncode not in (0, 1):
            raise ValueError("cannot inspect the previous commit")
        parents = ["HEAD"] if head.returncode == 0 else []
        # A merge inherits already committed assets from both parents. Only
        # exact parent blobs qualify; editing an exception cannot admit new
        # bytes. The index manifest still independently pins every asset.
        merge_path = Path(git(root, "rev-parse", "--git-path", "MERGE_HEAD").decode().strip())
        if not merge_path.is_absolute():
            merge_path = root / merge_path
        try:
            parents.extend(merge_path.read_text().splitlines())
        except FileNotFoundError:
            pass
        for parent in parents:
            for record in git(root, "ls-tree", "-r", "-z", parent).split(b"\0"):
                if record:
                    metadata, path = record.split(b"\t", 1)
                    previous.setdefault(path.decode(), set()).add(metadata.decode().split()[2])
    failures = []
    with subprocess.Popen(
        ["git", "-C", str(root), "cat-file", "--batch"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE,
    ) as process:
        # Keep requests below Linux's minimum pipe capacity. Git may block on
        # a large response before reading the remaining requests, so writing an
        # unbounded list before reading would deadlock. Small batches remove a
        # flush/read round trip per blob while retaining one body in memory.
        for offset in range(0, len(entries), 32):
            batch = entries[offset:offset + 32]
            process.stdin.write(b"".join((oid + "\n").encode() for oid, _ in batch))
            process.stdin.flush()
            for oid, path in batch:
                header = process.stdout.readline().split()
                if len(header) != 3 or header[:2] != [oid.encode(), b"blob"]:
                    raise ValueError("cannot inspect blob: " + path)
                size = int(header[2])
                if size < 0:
                    raise ValueError("invalid blob size: " + path)
                data = process.stdout.read(size)
                if len(data) != size or process.stdout.read(1) != b"\n":
                    raise ValueError("incomplete blob: " + path)
                issues = content_issues(data)
                changed_exception = staged and issues and oid not in previous.get(path, set())
                if issues and (allowed.get(path) != oid or changed_exception):
                    failures.append(path + ": " + ", ".join(issues) + ("; new/modified non-text or non-LF blobs are forbidden" if changed_exception else ""))
        process.stdin.close()
        if process.wait() != 0:
            raise ValueError("Git blob reader failed")
    return failures, len(entries)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--staged", action="store_true")
    args = parser.parse_args()
    try:
        failures, count = audit(ROOT, args.staged)
    except (ValueError, KeyError, subprocess.CalledProcessError) as error:
        print("Source content inspection failed: " + str(error), file=sys.stderr)
        return 1
    if failures:
        print("\n".join(failures))
        return 1
    print(f"Source content OK: {count} inspected blobs; exact asset exceptions applied", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
