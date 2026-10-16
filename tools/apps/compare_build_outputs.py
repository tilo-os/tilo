#!/usr/bin/env python3
"""Compare two builds' output trees byte for byte and say where they differ.

    compare_build_outputs.py FIRST SECOND [--ignore NAME ...]

Every path under FIRST must exist under SECOND with the same type, permission
bits, symlink target and bytes, and the other way round. For a differing tar
archive (a .tapp, a .tar or .tar.gz) the report names the members that differ,
which for a tapp says whether it was the binary (blobs/<hash>), the manifest
or the signature. When diffoscope is on PATH it explains each differing file
too. Exits 1 on any difference, 0 when the trees are identical.
"""

from __future__ import annotations

import argparse
import hashlib
import os
import shutil
import stat
import subprocess
import sys
import tarfile
from pathlib import Path

ARCHIVES = (".tapp", ".tar", ".tar.gz", ".tgz")
DIFFOSCOPE_LINES = 200


def entries(root: Path, ignore: set[str]) -> dict[str, tuple[str, int, str]]:
    """Path → (kind, permission bits, symlink target or content digest)."""
    found = {}
    if not root.is_dir():
        raise ValueError(f"{root}: expected a build directory")

    def walk_error(error):
        raise error

    for directory, names, files in os.walk(root, onerror=walk_error):
        names[:] = sorted(name for name in names if name not in ignore)
        for name in names + sorted(name for name in files if name not in ignore):
            path = Path(directory) / name
            info = path.lstat()
            relative = path.relative_to(root).as_posix()
            if stat.S_ISLNK(info.st_mode):
                found[relative] = ("link", 0, os.readlink(path))
            elif stat.S_ISDIR(info.st_mode):
                found[relative] = ("dir", stat.S_IMODE(info.st_mode), "")
            elif stat.S_ISREG(info.st_mode):
                found[relative] = ("file", stat.S_IMODE(info.st_mode), digest(path))
            else:
                found[relative] = (f"special:{stat.S_IFMT(info.st_mode):o}",
                                   stat.S_IMODE(info.st_mode), str(info.st_rdev))
    return found


def digest(path: Path) -> str:
    sha = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            sha.update(block)
    return sha.hexdigest()


def members(path: Path) -> dict[str, str]:
    """Each member of a tar archive → its kind, mode, owner, time and content digest."""
    out = {}
    with tarfile.open(path) as archive:
        for member in archive:
            data = archive.extractfile(member) if member.isfile() else None
            content = hashlib.sha256(data.read()).hexdigest() if data else member.linkname
            out[member.name] = (f"type={member.type!r} mode={member.mode:o} owner={member.uid}:{member.gid} "
                                f"mtime={member.mtime} size={member.size} sha256={content}")
    return out


def explain(first: Path, second: Path) -> list[str]:
    """Why two differing files differ, member by member for archives."""
    lines = []
    if first.name.endswith(ARCHIVES):
        try:
            a, b = members(first), members(second)
        except (tarfile.TarError, OSError) as error:
            lines.append(f"    not readable as an archive: {error}")
        else:
            for name in sorted(set(a) | set(b)):
                if a.get(name) != b.get(name):
                    lines.append(f"    member {name}: {a.get(name, 'absent')} | {b.get(name, 'absent')}")
            if not lines:
                lines.append("    same members; the archive framing differs")
    else:
        with first.open("rb") as left, second.open("rb") as right:
            offset = 0
            while True:
                x, y = left.read(1 << 16), right.read(1 << 16)
                if x != y:
                    index = next((i for i, (p, q) in enumerate(zip(x, y)) if p != q), min(len(x), len(y)))
                    lines.append(f"    first differing byte at offset {offset + index}")
                    break
                if not x:
                    break
                offset += len(x)
    diffoscope = shutil.which("diffoscope")
    if diffoscope:
        result = subprocess.run([diffoscope, "--text", "-", str(first), str(second)],
                                capture_output=True, text=True, check=False)
        lines.extend("    " + line for line in result.stdout.splitlines()[:DIFFOSCOPE_LINES])
    return lines


def compare(first: Path, second: Path, ignore: set[str]) -> list[str]:
    a, b = entries(first, ignore), entries(second, ignore)
    if not a:
        return [f"{first} holds nothing to compare"]
    problems = []
    for path in sorted(set(a) | set(b)):
        if path not in b:
            problems.append(f"{path}: only in the first build")
        elif path not in a:
            problems.append(f"{path}: only in the second build")
        elif a[path] != b[path]:
            problems.append(f"{path}: {a[path][0]} {a[path][1]:o} {a[path][2]} | {b[path][0]} {b[path][1]:o} {b[path][2]}")
            if a[path][0] == b[path][0] == "file":
                problems.extend(explain(first / path, second / path))
    return problems


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("first", type=Path)
    parser.add_argument("second", type=Path)
    parser.add_argument("--ignore", action="append", default=[], help="a file or directory name to skip anywhere")
    args = parser.parse_args(argv)
    try:
        problems = compare(args.first, args.second, set(args.ignore))
    except (OSError, ValueError) as error:
        print(f"build comparison failed: {error}", file=sys.stderr)
        return 2
    if problems:
        print(f"{args.first} and {args.second} differ:", file=sys.stderr)
        for line in problems:
            print(line, file=sys.stderr)
        return 1
    count = len(entries(args.first, set(args.ignore)))
    print(f"{count} paths identical in {args.first} and {args.second}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
