#!/usr/bin/env python3
"""Hard-link the identical regular files of a tree to one inode each.

    hardlink-identical.py ROOT [ROOT...]

An image layer carries each inode's bytes once, however many names point at
it, but every separate inode in full: the same 2.6 MB LC_COLLATE under 19
locales, the same mono assemblies under three API directories, were each a
copy in every pull (TIL-736). The image build runs this over a tree it writes
in one RUN, before the COPY that takes the tree into the rootfs, so the layer
holds one copy. A hard link is a plain file to every reader: unlike a symlink
it does not dangle in a sandbox that mounts only part of the tree, under
Flatpak or under Landlock.

Files are merged only when everything a reader could observe is the same:
bytes, mode, owner and extended attributes (file capabilities, SELinux
labels). Empty files are left alone. The choice is deterministic: the inode
whose first name sorts first keeps its bytes, and every name of every other
identical inode is re-pointed at it, so names that already shared an inode
stay together. Every replacement is atomic (link to a temporary name, rename
over the file). Prints the bytes the trees no longer carry twice.
"""

from __future__ import annotations

import collections
import hashlib
import os
import stat
import sys
from pathlib import Path

CHUNK = 1 << 20


def xattrs(path: str) -> tuple[tuple[str, bytes], ...]:
    if not hasattr(os, "listxattr"):
        return ()
    try:
        names = os.listxattr(path, follow_symlinks=False)
    except OSError:
        return ()
    return tuple(sorted((name, os.getxattr(path, name, follow_symlinks=False)) for name in names))


def digest(path: str) -> str:
    sha = hashlib.sha256()
    with open(path, "rb") as stream:
        while chunk := stream.read(CHUNK):
            sha.update(chunk)
    return sha.hexdigest()


def inodes(roots: list[str]) -> dict[tuple[int, int], list[str]]:
    """Every regular, non-empty file under `roots`, by inode, names sorted."""
    found: dict[tuple[int, int], list[str]] = collections.defaultdict(list)
    for root in roots:
        for directory, subdirectories, files in os.walk(root):
            subdirectories.sort()
            for name in sorted(files):
                path = os.path.join(directory, name)
                info = os.lstat(path)
                if stat.S_ISREG(info.st_mode) and info.st_size:
                    found[(info.st_dev, info.st_ino)].append(path)
    for names in found.values():
        names.sort()
    return found


def relink(canonical: str, path: str) -> None:
    temporary = os.path.join(os.path.dirname(path), f".{os.path.basename(path)}.hardlink-identical")
    os.link(canonical, temporary)
    os.replace(temporary, path)


def merge(roots: list[str]) -> int:
    """Hard-link identical files under `roots`; return the bytes saved."""
    groups: dict[tuple, list[tuple[str, list[str]]]] = collections.defaultdict(list)
    for names in inodes(roots).values():
        first = names[0]
        info = os.lstat(first)
        # A hard link cannot cross a filesystem; st_dev keeps such pairs apart.
        key = (info.st_size, info.st_dev, stat.S_IMODE(info.st_mode), info.st_uid, info.st_gid, xattrs(first))
        groups[key].append((first, names))
    saved = 0
    for key, members in groups.items():
        if len(members) < 2:
            continue
        by_content: dict[str, list[tuple[str, list[str]]]] = collections.defaultdict(list)
        for first, names in members:
            by_content[digest(first)].append((first, names))
        for same in by_content.values():
            if len(same) < 2:
                continue
            same.sort()
            canonical = same[0][0]
            for _, names in same[1:]:
                for name in names:
                    relink(canonical, name)
                saved += key[0]
    return saved


def main(argv: list[str]) -> int:
    roots = argv[1:]
    if not roots:
        print(__doc__.strip().splitlines()[2].strip(), file=sys.stderr)
        return 2
    for root in roots:
        if not Path(root).is_dir():
            print(f"hardlink-identical: {root} is not a directory", file=sys.stderr)
            return 1
    saved = merge(roots)
    print(f"hardlink-identical: {saved} bytes under {' '.join(roots)} now share an inode with an identical file")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
