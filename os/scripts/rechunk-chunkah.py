#!/usr/bin/env python3
"""Run chunkah over the OS image's root, giving some trees layers of their own (TIL-747).

chunkah groups files by RPM package, puts files over 1 MB on their own, and
packs every other file into one "unclaimed" component, which is one layer.
In the OS image most of those bytes are Proton's and the Steam runtime's small
files under /usr/lib/tilo/windows-runners (841 of 961 MB on x86_64), which
change when their pins move, beside Tilo's own small files, which change
with almost every build: one changed font catalog re-sent the whole layer.

So each tree in TREES is chunked by a run of its own, over the same root with
everything outside the tree pruned, into at most TREE_LAYERS layers, and the
main run prunes the tree's contents. Each set of files in FILE_SETS gets one
layer the same way, and the main run prunes those files.
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

# Trees that change on their own schedule, by name. Paths are absolute in the image.
TREES = {
    "windows-runners": "/usr/lib/tilo/windows-runners",
    # The preinstalled apps: each app's binary and signed archive, which change
    # only with that app. In the main run they shared a few large
    # layers with each other and with the rest of the image, so one changed
    # app re-sent two of them, 76 MB; split finer, it re-sends a few MB.
    "app-store": "/usr/share/tilo-apps",
}
# Layers a tree's own run may pack its components into.
TREE_LAYERS = {"windows-runners": 16, "app-store": 32}

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


def main(argv: list[str]) -> int:
    rootfs, out, max_layers = Path(argv[1]), Path(argv[2]), int(argv[3])
    if int(os.environ.get("SOURCE_DATE_EPOCH", "0")) <= 0:
        raise SystemExit("SOURCE_DATE_EPOCH must be a positive epoch")
    trees = {name: path for name, path in TREES.items() if (rootfs / path.lstrip("/")).is_dir()}
    file_sets = {name: files for name, patterns in FILE_SETS.items() if (files := matches(rootfs, patterns))}
    set_files = [path for files in file_sets.values() for path in files]
    # /sysroot/: the base's ostree repository, which bootc's composefs backend
    # never deploys. A trailing slash keeps the directory itself.
    main_prune = ["/sysroot/", *(path + "/" for path in trees.values()),
                  *(path for path in set_files if not within(path, trees.values()))]
    out.mkdir(parents=True, exist_ok=True)
    chunkah(rootfs, out / "main", max_layers, main_prune)
    (out / "main" / "image").rename(out / "image")
    (out / "main" / "components.json").rename(out / "components.json")
    (out / "main").rmdir()
    for name, path in trees.items():
        # A file set's files inside the tree are the set's, not the tree's.
        prune = outside(rootfs, [path]) + [file for file in set_files if within(file, [path])]
        chunkah(rootfs, out / "trees" / name, TREE_LAYERS[name], prune)
    for name, files in file_sets.items():
        chunkah(rootfs, out / "trees" / name, 1, outside(rootfs, files))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
