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
    """HEAD's tree, or the index's in a repository without commits."""
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


def staged_entries(root):
    """The index entries this commit changes against HEAD, with their modes.

    A file the commit leaves as HEAD has it was judged by the commit that added
    it, and the whole tree is judged by the policy suite and by CI on every
    pushed head. Reading only the change is what keeps the hook's cost with the
    size of the commit instead of the size of the tree.
    """
    output = git(root, "diff", "--cached", "--raw", "-z", "--no-abbrev", "--diff-filter=ACMR")
    fields = output.split(b"\0")
    entries = []
    index = 0
    while index < len(fields):
        header = fields[index]
        index += 1
        if not header.startswith(b":"):
            continue
        parts = header[1:].split()
        if len(parts) != 5:
            continue
        _old_mode, new_mode, _old_oid, new_oid, status = (part.decode() for part in parts)
        if status[:1] in ("R", "C"):
            # A rename or copy names its source first; the entry is the destination.
            index += 1
        if index >= len(fields):
            break
        entries.append((new_oid, fields[index].decode(), new_mode))
        index += 1
    return entries


def inherited_ids(root, parents, path):
    """The blob ids ``parents`` hold for ``path``: a merge inherits its assets.

    Only an exact parent blob is inherited, so editing an exception cannot
    admit new bytes.
    """
    found = set()
    for parent in parents:
        try:
            found.add(git(root, "rev-parse", "--verify", "--quiet", f"{parent}:{path}").decode().strip())
        except subprocess.CalledProcessError:
            continue
    return found


def pinned_ids(root, parents):
    """The blob ids the parents' own exception manifests pin, at whatever path.

    A pinned asset that moves keeps its exact bytes, so it admits nothing new.
    """
    found = set()
    for parent in parents:
        try:
            manifest = git(root, "show", f"{parent}:{EXCEPTIONS}")
        except subprocess.CalledProcessError:
            continue
        found.update(entry["oid"] for entry in json.loads(manifest))
    return found


def audit(root, staged=False):
    if staged:
        entries = []
        for oid, path, mode in staged_entries(root):
            if mode == "160000":
                raise ValueError("uninspected submodule: " + path)
            entries.append((oid, path))
        manifest = git(root, "show", ":" + EXCEPTIONS)
    else:
        tree = committed_tree(root)
        entries = []
        for entry in git(root, "ls-tree", "-r", "-z", tree).split(b"\0"):
            if not entry:
                continue
            metadata, path = entry.split(b"\t", 1)
            mode, _kind, oid = metadata.decode().split()
            if mode == "160000":
                raise ValueError("uninspected submodule: " + path.decode())
            entries.append((oid, path.decode()))
        manifest = git(root, "show", tree + ":" + EXCEPTIONS)
    exceptions = json.loads(manifest)
    allowed = {}
    for entry in exceptions:
        key = entry["path"]
        if key in allowed or not entry["reason"].strip():
            raise ValueError("duplicate or unexplained asset exception: " + key)
        allowed[key] = entry["oid"]
    parents = []
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
    pinned = pinned_ids(root, parents)
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
                changed_exception = (
                    staged and issues and oid not in pinned and oid not in inherited_ids(root, parents, path)
                )
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
