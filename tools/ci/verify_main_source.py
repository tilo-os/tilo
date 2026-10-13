#!/usr/bin/env python3
"""Verify a pinned image source in the native planner's 300 main states."""
import argparse
import json
import os
import re
import sys
import urllib.request

LIMIT = 300
MAX_RESPONSE_BYTES = 4 * 1024 * 1024
SHA = re.compile(r"[0-9a-f]{40}\Z")


def first_parent_member(source, head, read_commit):
    """Only first parents were main states; a merged side parent is not one."""
    if not SHA.fullmatch(source) or not SHA.fullmatch(head):
        raise ValueError("source and head must be full commit IDs")
    cursor = head
    visited = set()
    for _ in range(LIMIT):
        if cursor == source:
            return True
        if cursor in visited:
            raise ValueError("cyclic commit metadata")
        visited.add(cursor)
        commit = read_commit(cursor)
        if not isinstance(commit, dict) or commit.get("sha") != cursor or not isinstance(commit.get("parents"), list):
            raise ValueError("incomplete commit metadata")
        if not commit["parents"]:
            return False
        parent = commit["parents"][0]
        cursor = parent.get("sha", "") if isinstance(parent, dict) else ""
        if not isinstance(cursor, str) or not SHA.fullmatch(cursor):
            raise ValueError("missing first parent")
    raise ValueError("source is outside the last 300 first-parent main states")


def read_commit(sha):
    server = os.environ["GITHUB_SERVER_URL"].rstrip("/")
    repo = os.environ["GITHUB_REPOSITORY"]
    request = urllib.request.Request(
        f"{server}/api/v1/repos/{repo}/git/commits/{sha}",
        headers={"Authorization": f"token {os.environ['TOKEN']}"},
    )
    with urllib.request.urlopen(request, timeout=15) as response:
        raw = response.read(MAX_RESPONSE_BYTES + 1)
    if len(raw) > MAX_RESPONSE_BYTES:
        raise ValueError("commit response exceeds bound")
    return json.loads(raw)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", required=True)
    parser.add_argument("--head", required=True)
    args = parser.parse_args()
    try:
        if not first_parent_member(args.source, args.head, read_commit):
            raise ValueError("source was not a first-parent main state")
    except (OSError, ValueError, KeyError) as error:
        print(f"::error::cannot verify pinned main source ({type(error).__name__})", file=sys.stderr)
        return 1
    print(f"::notice::verified first-parent main source {args.source}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
