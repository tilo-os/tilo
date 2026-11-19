#!/usr/bin/env python3
"""Label a system extension image and refuse one that would change /usr.

    sysext-image.py label IMAGE TREE [--contexts FILE_CONTEXTS]
    sysext-image.py check IMAGE [--base ROOT]

systemd-sysext merges an image's usr/ over /usr as an overlay, and for a
directory both have, the overlay shows the image's owner, mode and SELinux
label. A payload unpacked from upstream archives carries whatever its builders
had: a /usr owned by a build user, or one without labels, which the kernel then
reports as unlabeled_t, keeps every login out and stops the boot in
systemd-tmpfiles-setup.

`label` gives every entry of the ext4 IMAGE (made from TREE by mkfs.ext4 -d)
the SELinux label the base's file contexts give its path, as setfiles would
on the installed system. `check` reads the image back and refuses it unless
every entry is owned by root, writable by its owner alone, sets no user or
group id on execution and carries a label, and every directory ROOT (the base
the image was built in) also has keeps ROOT's mode and stays a directory.
Both read and write the image with debugfs, without mounting it, so they work
inside a container build. Exits 1 and names what is wrong.
"""

from __future__ import annotations

import argparse
import os
import stat
import subprocess
import sys
import tempfile
from dataclasses import dataclass

CONTEXTS = "/etc/selinux/targeted/contexts/files/file_contexts"
REFUSED_LABELS = (":unlabeled_t:", ":file_t:")
PROMPT = "debugfs: "
LIMIT = 20
TYPES = {stat.S_IFDIR: "dir", stat.S_IFLNK: "lnk_file", stat.S_IFREG: "file"}


@dataclass(frozen=True)
class Entry:
    path: str
    mode: int
    uid: int
    gid: int
    label: str | None


def debugfs(image: str, commands: list[str], write: bool = False) -> str:
    """The output of debugfs running COMMANDS on IMAGE, each echoed after PROMPT."""
    with tempfile.NamedTemporaryFile("w", prefix="debugfs-", suffix=".txt") as script:
        script.write("".join(command + "\n" for command in commands))
        script.flush()
        argv = ["debugfs", *(["-w"] if write else []), "-f", script.name, image]
        return subprocess.check_output(argv, stderr=subprocess.PIPE, text=True,
                                       errors="surrogateescape")


def answers(output: str) -> dict[str, list[str]]:
    """Each echoed command of a debugfs script with the lines it printed."""
    found: dict[str, list[str]] = {}
    current = None
    for line in output.splitlines():
        if line.startswith(PROMPT):
            current = line[len(PROMPT):]
            found[current] = []
        elif current is not None and line:
            found[current].append(line)
    return found


def listing(lines: list[str]):
    """(inode, mode, uid, gid, name) of each line `ls -p` printed: /ino/mode/uid/gid/name/size/."""
    for line in lines:
        fields = line.split("/")
        if len(fields) != 8 or fields[0] or fields[7]:
            raise ValueError(f"unreadable listing line {line!r}")
        yield int(fields[1]), int(fields[2], 8), int(fields[3]), int(fields[4]), fields[5]


def entries(image: str) -> list[Entry]:
    """Every entry of IMAGE reachable from its root, with owner, mode and label."""
    found: dict[int, tuple[str, int, int, int]] = {}
    level = [(2, "/")]
    while level:
        output = answers(debugfs(image, [f"ls -p <{ino}>" for ino, _ in level]))
        following = []
        for ino, path in level:
            for child, mode, uid, gid, name in listing(output[f"ls -p <{ino}>"]):
                if name == "." and ino == 2:
                    found[2] = ("/", mode, uid, gid)
                if name in (".", "..") or child == 0:
                    continue
                child_path = os.path.join(path, name)
                found[child] = (child_path, mode, uid, gid)
                if stat.S_ISDIR(mode):
                    following.append((child, child_path))
        level = following
    command = "ea_get -V <{}> security.selinux"
    labels = answers(debugfs(image, [command.format(ino) for ino in found]))
    result = []
    for ino, (path, mode, uid, gid) in found.items():
        printed = labels.get(command.format(ino), [])
        label = printed[0].rstrip("\0") if printed else None
        result.append(Entry(path, mode, uid, gid, label))
    return sorted(result, key=lambda entry: entry.path)


def problems(found: list[Entry], base: str | None):
    # The image's own root is never merged: systemd-sysext takes its usr/ alone.
    for entry in (entry for entry in found if entry.path != "/"):
        kind = stat.S_IFMT(entry.mode)
        if (entry.uid, entry.gid) != (0, 0):
            yield f"{entry.path} is owned by {entry.uid}:{entry.gid}, not root"
        if kind != stat.S_IFLNK and entry.mode & 0o6022:
            yield f"{entry.path} has mode {entry.mode & 0o7777:04o}: writable by others or set-id"
        if not entry.label:
            yield f"{entry.path} has no SELinux label"
        elif any(refused in entry.label for refused in REFUSED_LABELS):
            yield f"{entry.path} is labelled {entry.label}"
        if base is None:
            continue
        try:
            there = os.lstat(os.path.join(base, entry.path.lstrip("/")))
        except FileNotFoundError:
            continue
        if stat.S_ISDIR(there.st_mode) != (kind == stat.S_IFDIR):
            yield f"{entry.path} would replace the base's {stat.filemode(there.st_mode)} entry"
        elif kind == stat.S_IFDIR and stat.S_IMODE(there.st_mode) != stat.S_IMODE(entry.mode):
            yield (f"{entry.path} has mode {stat.S_IMODE(entry.mode):04o}, "
                   f"the base's has {stat.S_IMODE(there.st_mode):04o}")


def check(image: str, base: str | None) -> list[str]:
    found = entries(image)
    if not any(entry.path == "/usr" for entry in found):
        return ["the image has no /usr"]
    refused = list(problems(found, base))
    if not refused:
        print(f"sysext-image: {len(found)} entries, owned by root and labelled")
    return refused


def labels(tree: str, contexts: str) -> dict[str, str]:
    """The file contexts' label for each path of TREE, as installed at /PATH, and
    for the lost+found directory mkfs.ext4 adds."""
    by_type: dict[str, list[str]] = {"dir": ["/lost+found"]}
    for top, dirs, files in os.walk(tree):
        for name in [*dirs, *files, *([] if top != tree else ["."])]:
            path = os.path.normpath(os.path.join(top, name))
            installed = "/" + os.path.relpath(path, tree).removeprefix(".")
            if "\n" in installed or "\t" in installed or '"' in installed:
                raise ValueError(f"{installed!r}: a name debugfs and matchpathcon cannot carry")
            kind = TYPES.get(stat.S_IFMT(os.lstat(path).st_mode))
            if kind is None:
                raise ValueError(f"{installed}: only directories, files and links may travel")
            by_type.setdefault(kind, []).append(os.path.normpath(installed))
    found = {}
    for kind, paths in by_type.items():
        for start in range(0, len(paths), 500):
            batch = paths[start:start + 500]
            printed = subprocess.check_output(
                ["matchpathcon", "-N", "-f", contexts, "-m", kind, *batch],
                stderr=subprocess.PIPE, text=True)
            for line in printed.splitlines():
                path, _, label = line.partition("\t")
                found[path] = label
            missing = [path for path in batch if not found.get(path)]
            if missing:
                raise ValueError(f"no label in {contexts} for {missing[0]}")
    return found


def label(image: str, tree: str, contexts: str) -> None:
    commands = [f'ea_set "{path}" security.selinux {value}'
                for path, value in sorted(labels(tree, contexts).items())]
    output = debugfs(image, commands, write=True)
    errors = [line for line in output.splitlines() if line and not line.startswith(PROMPT)]
    if errors:
        raise ValueError(f"debugfs could not label the image: {errors[0]}")


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    commands = parser.add_subparsers(dest="command", required=True)
    labelling = commands.add_parser("label")
    labelling.add_argument("image")
    labelling.add_argument("tree")
    labelling.add_argument("--contexts", default=CONTEXTS)
    checking = commands.add_parser("check")
    checking.add_argument("image")
    checking.add_argument("--base", help="the root the image is merged over (its build base)")
    args = parser.parse_args(argv)
    try:
        if args.command == "label":
            label(args.image, args.tree, args.contexts)
            return 0
        found = check(args.image, args.base)
    except (ValueError, KeyError, StopIteration, subprocess.CalledProcessError) as error:
        print(f"sysext-image: {args.image}: {error}", file=sys.stderr)
        return 1
    for line in found[:LIMIT]:
        print(f"sysext-image: {line}", file=sys.stderr)
    if len(found) > LIMIT:
        print(f"sysext-image: and {len(found) - LIMIT} more", file=sys.stderr)
    return 1 if found else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
