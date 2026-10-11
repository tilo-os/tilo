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
main run prunes the tree's contents. A tree is a list of glob patterns
(absolute image paths); a tree whose path lies inside another tree's is
pruned from that one. Each set of files in FILE_SETS gets one layer the same
way, and every other run prunes those files.
os/scripts/oci-rechunk.py merges the layouts and proves the result is the raw
image.

    rechunk-chunkah.py ROOTFS OUT MAX_LAYERS

writes OUT/image (the main layout), OUT/components.json, and for each tree
or file set present in ROOTFS, OUT/trees/<name>/image and its components.json.
SOURCE_DATE_EPOCH must be set; chunkah reads it.
"""

from __future__ import annotations

import glob
import os
import subprocess
import sys
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
    "tilo": ["/usr/bin/shell", "/usr/bin/fs", "/usr/bin/app", "/usr/bin/tilo-*", "/usr/libexec/tilo*",
             "/usr/lib/tilo", "/usr/share/tilo", "/boot/EFI/Linux"],
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

    Directories (not links to one) are trees; files and links are single
    entries. A path below a directory of the same tree is that directory's.
    """
    found = set()
    for pattern in patterns:
        for match in glob.glob(str(rootfs / pattern.lstrip("/"))):
            found.add("/" + Path(match).relative_to(rootfs).as_posix())
    return sorted(path for path in found if not within(path, found))


def is_tree(rootfs: Path, path: str) -> bool:
    """Whether the image path `path` is a directory in `rootfs`, not a link to one."""
    full = rootfs / path.lstrip("/")
    return full.is_dir() and not full.is_symlink()


def pruned(rootfs: Path, path: str) -> str:
    """How a run leaves `path` to another: a directory keeps itself, so a trailing slash; anything else goes."""
    return path + "/" if is_tree(rootfs, path) else path


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


def main(argv: list[str]) -> int:
    rootfs, out, max_layers = Path(argv[1]), Path(argv[2]), int(argv[3])
    if int(os.environ.get("SOURCE_DATE_EPOCH", "0")) <= 0:
        raise SystemExit("SOURCE_DATE_EPOCH must be a positive epoch")
    trees = tree_roots(rootfs)
    every_root = [path for paths in trees.values() for path in paths]
    file_sets = {name: files for name, patterns in FILE_SETS.items() if (files := matches(rootfs, patterns))}
    set_files = [path for files in file_sets.values() for path in files]
    # /sysroot/: the base's ostree repository, which bootc's composefs backend
    # never deploys. A trailing slash keeps the directory itself.
    main_prune = ["/sysroot/", *(pruned(rootfs, path) for path in every_root if not within(path, every_root)),
                  *(path for path in set_files if not within(path, every_root))]
    out.mkdir(parents=True, exist_ok=True)
    chunkah(rootfs, out / "main", max_layers, main_prune)
    (out / "main" / "image").rename(out / "image")
    (out / "main" / "components.json").rename(out / "components.json")
    (out / "main").rmdir()
    for name, paths in trees.items():
        # Another tree inside this one is that tree's; a file set's files are the set's.
        nested = [path for path in every_root if path not in paths and within(path, paths)]
        prune = (outside(rootfs, paths) + [pruned(rootfs, path) for path in nested]
                 + [file for file in set_files if within(file, paths) and not within(file, nested)])
        chunkah(rootfs, out / "trees" / name, TREE_LAYERS[name], prune)
    for name, files in file_sets.items():
        chunkah(rootfs, out / "trees" / name, 1, outside(rootfs, files))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
