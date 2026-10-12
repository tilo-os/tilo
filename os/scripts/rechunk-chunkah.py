#!/usr/bin/env python3
"""Run chunkah over the OS image's root, giving some trees layers of their own (TIL-747).

chunkah groups files by RPM package, puts files over 1 MB on their own, and
packs every other file into one "unclaimed" component, which is one layer.
Which components share a layer depends on every component in the run: the
largest become layers of their own up to 80 % of the budget, the rest are
hashed into bins. So one new or removed component anywhere in a run can move
unchanged ones into other layers, and one changed file re-sends every file
its layer shares.

So each tree in TREES is chunked by a run of its own, over the same root with
everything outside the tree pruned, into at most TREE_LAYERS layers, and the
main run prunes the tree, its top directories included. A tree is a list of glob patterns
(absolute image paths); a tree whose path lies inside another tree's is
pruned from that one. Each set of files in FILE_SETS gets one layer the same
way, and every other run prunes those files. The files no RPM owns that are
in no tree or set get a run of their own too ("unowned"), so the main run
holds packages only.
os/scripts/oci-rechunk.py merges the layouts and proves the result is the raw
image.

    rechunk-chunkah.py ROOTFS OUT MAX_LAYERS

writes OUT/image (the main layout), OUT/components.json, and for each tree
or file set present in ROOTFS, OUT/trees/<name>/image and its components.json.
SOURCE_DATE_EPOCH must be set; chunkah reads it. `rpm` must be on PATH: the
unowned files are read from ROOTFS's own package database.
"""

from __future__ import annotations

import glob
import os
import shutil
import stat
import subprocess
import sys
import tempfile
from collections.abc import Iterable
from pathlib import Path

# Trees that change on their own schedule, by name.
TREES = {
    # Proton and the Steam runtime: change when their pins move.
    "windows-runners": ["/usr/lib/tilo/windows-runners"],
    # The preinstalled apps: each app's binary and signed archive, which change
    # only with that app. In the main run they shared a few large
    # layers with each other and with the rest of the image, so one changed
    # app re-sent two of them, 76 MB; split finer, it re-sends a few MB.
    "app-store": ["/usr/share/tilo-apps"],
    # The native runtime is named by its payload's digest
    # (<release>+native.<digest>), so one changed library renames every file
    # in it, the fonts' other names (hard links) included. In the main run its
    # renamed files re-sent the layers they shared with Chromium's .pak files
    # and with the image's small files: 55 MB and 36 MB for 15 MB and 22 MB of
    # runtime. Here a rename re-sends the runtime alone.
    "native-runtime": ["/usr/lib/tilo-runtime/*+native.*", "/usr/share/fonts/tilo",
                       "/usr/share/tilo/font-catalogs"],
    # The Chromium engine: changes with its version only.
    "browser-runtime": ["/usr/lib/tilo-runtime/*-chromium"],
    # Tilo's own programs and data, which change with Tilo's commits. Among
    # the main run's components a new or resized binary moved RPMs between
    # layers (21 MB of unchanged packages re-sent); on their own, with a budget
    # above their count, each binary is one layer and nothing is packed.
    "tilo": ["/usr/bin/shell", "/usr/bin/fs", "/usr/bin/app", "/usr/bin/tilo", "/usr/bin/tilo-*",
             "/usr/libexec/tilo*", "/usr/lib/tilo", "/usr/share/tilo", "/boot/EFI/Linux"],
}
# Layers a tree's own run may pack its components into. A runtime changes as
# a whole, so few layers serve it; the tilo tree has ~40 components and must
# stay above that, or chunkah packs them and one new binary moves the rest.
TREE_LAYERS = {"windows-runners": 16, "app-store": 32, "native-runtime": 4, "browser-runtime": 4, "tilo": 56}

# Files that change with every build, or with any app, although they are small,
# by name; patterns are absolute in the image. Among Tilo's small files any one
# of them renews the whole unclaimed layer, tens of MB, in every update; on
# their own each set is one layer of a few hundred KB.
FILE_SETS = {
    # The runtime bundles are signed by keys minted per build, because the
    # launcher trusts those keys to mount code into every app
    # (os/Containerfile), so each build's bundle.toml, publisher keys and trust
    # list differ; systemd's update stamp records the build's time.
    "build-signatures": [
        "/usr/lib/tilo-runtime/*/bundle.toml",
        "/usr/share/tilo/*-publisher.hex",
        "/etc/tilo/runtime-bundle-trusted-publishers.txt",
        "/etc/.updated",
        "/var/.updated",
    ],
    # The app store's index: one row and one metadata directory per app, new
    # whenever any app is.
    "app-store-index": [
        "/usr/share/tilo-apps/registry.toml",
        "/usr/share/tilo-apps/pkgs/*.pkg/*",
    ],
}


def outside(rootfs: Path, keep: list[str]) -> list[str]:
    """Every path beside the ancestors of the paths in `keep`: pruning them leaves those paths and their parents."""
    kept = {Path(path) for path in keep}
    ancestors = {parent for path in kept for parent in path.parents}
    pruned = []
    for parent in sorted(ancestors):
        for name in sorted(os.listdir(rootfs / parent.relative_to("/"))):
            child = parent / name
            if child not in kept and child not in ancestors:
                pruned.append(str(child))
    return pruned


def matches(rootfs: Path, patterns: list[str]) -> list[str]:
    """The regular files of `rootfs` that `patterns` name, as absolute image paths."""
    found = set()
    for pattern in patterns:
        for match in glob.glob(str(rootfs / pattern.lstrip("/"))):
            path = Path(match)
            if path.is_file() and not path.is_symlink():
                found.add("/" + path.relative_to(rootfs).as_posix())
    return sorted(found)


def roots(rootfs: Path, patterns: list[str]) -> list[str]:
    """The paths of `rootfs` that a tree's `patterns` name, as absolute image paths.

    A path below another path of the same tree is that path's.
    """
    found = set()
    for pattern in patterns:
        for match in glob.glob(str(rootfs / pattern.lstrip("/"))):
            found.add("/" + Path(match).relative_to(rootfs).as_posix())
    return sorted(path for path in found if not within(path, found))


def within(path: str, trees: Iterable[str]) -> bool:
    """Whether the image path `path` is below one of `trees`."""
    return any(Path(tree) in Path(path).parents for tree in trees)


def chunkah(rootfs: Path, out: Path, max_layers: int, prune: list[str]) -> None:
    out.mkdir(parents=True)
    command = ["chunkah", "build", "--rootfs", str(rootfs), "--max-layers", str(max_layers), "--compressed",
               "--write-manifest-to", str(out / "components.json"), "--output", f"oci:{out / 'image'}"]
    for path in prune:
        command += ["--prune", path]
    subprocess.run(command, check=True)


def tree_roots(rootfs: Path) -> dict[str, list[str]]:
    """Each tree's paths in `rootfs`, for the trees present; a path two trees name is an error.

    A path may lie inside another tree's directory: it is then pruned from
    that tree's run (the fonts' catalogue inside /usr/share/tilo, Proton
    inside /usr/lib/tilo).
    """
    trees = {name: paths for name, patterns in TREES.items() if (paths := roots(rootfs, patterns))}
    owner: dict[str, str] = {}
    for name, paths in trees.items():
        for path in paths:
            if path in owner:
                raise SystemExit(f"{path} is in two trees: {owner[path]} and {name}")
            owner[path] = name
    return trees


# Layers for the files no package owns (below). Its bigfiles (the SELinux
# policy, the rpmdb, PDFium) are layers of their own and its small files one
# more, so a changed unit re-sends the unit's layer and nothing else.
UNOWNED_LAYERS = 8
# Where an image keeps its package database, newest layout first.
RPMDB_PATHS = ("/usr/lib/sysimage/rpm", "/usr/share/rpm", "/var/lib/rpm")


def rpmdb(rootfs: Path) -> str:
    """The image path of `rootfs`'s package database; an image without one is an error.

    A path that is a symlink is passed over: rpm would resolve an absolute
    target outside `rootfs`.
    """
    for path in RPMDB_PATHS:
        directory = rootfs / path.lstrip("/")
        if not directory.is_symlink() and directory.is_dir() and any(
                (directory / name).is_file() for name in ("rpmdb.sqlite", "Packages", "Packages.db")):
            return path
    raise SystemExit(f"{rootfs} has no package database in {', '.join(RPMDB_PATHS)}")


def owned_inodes(rootfs: Path) -> set[tuple[int, int]]:
    """The inodes of every path an RPM of `rootfs` owns, so a hard link to one counts as owned.

    rpm reads a copy of the database: opening the image's own could leave
    SQLite sidecars in the root that every later run would then chunk.
    """
    with tempfile.TemporaryDirectory() as scratch:
        database = Path(scratch) / "rpm"
        shutil.copytree(rootfs / rpmdb(rootfs).lstrip("/"), database, symlinks=True)
        listing = subprocess.run(["rpm", "--dbpath", str(database), "-qa", "--qf", "[%{FILENAMES}\\n]"],
                                 check=True, capture_output=True, text=True).stdout
    owned = set()
    for line in listing.splitlines():
        if not line.startswith("/"):
            continue
        try:
            status = os.lstat(rootfs / line.lstrip("/"))
        except (FileNotFoundError, NotADirectoryError):
            continue
        owned.add((status.st_dev, status.st_ino))
    if not owned:
        raise SystemExit(f"the package database of {rootfs} owns no file it carries")
    return owned


def unowned(rootfs: Path, skip: list[str]) -> list[str]:
    """The regular files and symlinks of `rootfs` that no RPM owns, outside `skip`, as image paths.

    In the main run chunkah packs them into one small "unclaimed" component
    that its budget hashes into a bin with RPMs, so any change to one of Tilo's
    units or config files re-sent tens of MB of unchanged packages.
    """
    owned = owned_inodes(rootfs)
    skipped = set(skip)
    found = []
    for directory, subdirectories, files in os.walk(rootfs):
        relative = Path(directory).relative_to(rootfs).as_posix()
        prefix = "/" if relative == "." else f"/{relative}/"
        kept = []
        for name in sorted(subdirectories):
            path = prefix + name
            if path in skipped:
                continue
            if (rootfs / path.lstrip("/")).is_symlink():
                files.append(name)
            else:
                kept.append(name)
        subdirectories[:] = kept
        for name in sorted(files):
            path = prefix + name
            status = os.lstat(rootfs / path.lstrip("/"))
            if path in skipped or (status.st_dev, status.st_ino) in owned:
                continue
            if stat.S_ISREG(status.st_mode) or stat.S_ISLNK(status.st_mode):
                found.append(path)
    return sorted(found)


def main(argv: list[str]) -> int:
    rootfs, out, max_layers = Path(argv[1]), Path(argv[2]), int(argv[3])
    if int(os.environ.get("SOURCE_DATE_EPOCH", "0")) <= 0:
        raise SystemExit("SOURCE_DATE_EPOCH must be a positive epoch")
    trees = tree_roots(rootfs)
    every_root = [path for paths in trees.values() for path in paths]
    file_sets = {name: files for name, patterns in FILE_SETS.items() if (files := matches(rootfs, patterns))}
    set_files = [path for files in file_sets.values() for path in files]
    # /sysroot/: the base's ostree repository, which bootc's composefs backend
    # never deploys. A trailing slash keeps the directory itself. A tree's
    # paths go whole, directory included: its own run writes them, and a
    # directory named by a digest (the native runtime's) in another run would
    # renew that run's layer at every rename.
    main_prune = ["/sysroot/", *(path for path in every_root if not within(path, every_root)),
                  *(path for path in set_files if not within(path, every_root))]
    # Files no package owns leave the main run for one of their own, which
    # leaves the main run the RPMs alone.
    loose = unowned(rootfs, ["/sysroot", *every_root, *set_files])
    main_prune += loose
    out.mkdir(parents=True, exist_ok=True)
    chunkah(rootfs, out / "main", max_layers, main_prune)
    (out / "main" / "image").rename(out / "image")
    (out / "main" / "components.json").rename(out / "components.json")
    (out / "main").rmdir()
    for name, paths in trees.items():
        # Another tree inside this one is that tree's; a file set's files are the set's.
        nested = [path for path in every_root if path not in paths and within(path, paths)]
        prune = (outside(rootfs, paths) + nested
                 + [file for file in set_files if within(file, paths) and not within(file, nested)])
        chunkah(rootfs, out / "trees" / name, TREE_LAYERS[name], prune)
    for name, files in file_sets.items():
        chunkah(rootfs, out / "trees" / name, 1, outside(rootfs, files))
    if loose:
        chunkah(rootfs, out / "trees" / "unowned", UNOWNED_LAYERS, outside(rootfs, loose))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
