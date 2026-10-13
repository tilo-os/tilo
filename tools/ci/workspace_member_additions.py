#!/usr/bin/env python3
"""Select literal workspace members added without any other manifest changes.

Any ambiguous or structural change fails closed: callers must scan every crate.
The comparison uses both committed endpoints, not the checkout's working manifest.
"""

from __future__ import annotations

import argparse
from copy import deepcopy
from pathlib import Path, PurePosixPath
import subprocess
import sys
import tomllib


def additions(root: Path, revision_range: str) -> list[str]:
    """Return added crate directories, or raise when a full scan is required."""
    before, after = revision_range.split("..")
    manifests = []
    for revision in (before, after):
        source = subprocess.check_output(
            ["git", "show", f"{revision}:Cargo.toml"], cwd=root
        )
        manifests.append(tomllib.loads(source.decode()))
    old, new = manifests
    old_members = old["workspace"]["members"]
    new_members = new["workspace"]["members"]
    if not all(isinstance(members, list) and all(isinstance(p, str) for p in members)
               for members in (old_members, new_members)):
        raise ValueError("workspace members must be string arrays")
    if len(set(old_members)) != len(old_members) or len(set(new_members)) != len(new_members):
        raise ValueError("duplicate members are ambiguous")
    if [p for p in new_members if p in old_members] != old_members:
        raise ValueError("existing members were removed or reordered")
    old_without = deepcopy(old)
    new_without = deepcopy(new)
    del old_without["workspace"]["members"]
    del new_without["workspace"]["members"]
    if old_without != new_without:
        raise ValueError("other workspace manifest settings changed")
    added = [p for p in new_members if p not in old_members]
    if not added:
        raise ValueError("not a member addition")
    for member in added:
        path = PurePosixPath(member)
        if (path.is_absolute() or str(path) != member or ".." in path.parts
                or any(c in member for c in '*?[]\\\x00\n\r')
                or member == "." or member.startswith("vendor/")):
            raise ValueError("added member is not a literal first-party directory")
        manifest = root / member / "Cargo.toml"
        manifest.resolve().relative_to(root.resolve())
        if not manifest.is_file():
            raise ValueError("added member has no manifest")
    return added


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("revision_range")
    args = parser.parse_args()
    try:
        members = additions(args.root, args.revision_range)
    except (KeyError, TypeError, ValueError, OSError, subprocess.CalledProcessError):
        return 1
    for member in members:
        sys.stdout.buffer.write(member.encode() + b"\0")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
