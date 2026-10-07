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
        records = git(root, "ls-tree", "-r", "-z", "HEAD").split(b"\0")
        manifest = git(root, "show", "HEAD:" + EXCEPTIONS)
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
        if head.returncode == 0:
            for record in git(root, "ls-tree", "-r", "-z", "HEAD").split(b"\0"):
                if record:
                    metadata, path = record.split(b"\t", 1)
                    previous[path.decode()] = metadata.decode().split()[2]
    failures = []
    with subprocess.Popen(
        ["git", "-C", str(root), "cat-file", "--batch"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE,
    ) as process:
        for oid, path in entries:
            process.stdin.write((oid + "\n").encode())
            process.stdin.flush()
            header = process.stdout.readline().split()
            if len(header) != 3 or header[1] != b"blob":
                raise ValueError("cannot inspect blob: " + path)
            size = int(header[2])
            data = process.stdout.read(size)
            if len(data) != size or process.stdout.read(1) != b"\n":
                raise ValueError("incomplete blob: " + path)
            issues = content_issues(data)
            changed_exception = staged and issues and previous.get(path) != oid
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
        print("\n".join(failures), file=sys.stderr)
        return 1
    print(f"Source content OK: {count} inspected blobs; exact asset exceptions applied")
    return 0


if __name__ == "__main__":
    sys.exit(main())
