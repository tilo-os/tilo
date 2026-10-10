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
main run prunes the tree's contents. os/scripts/oci-rechunk.py merges the
layouts and proves the result is the raw image.

    rechunk-chunkah.py ROOTFS OUT MAX_LAYERS

writes OUT/image (the main layout), OUT/components.json, and for each tree
present in ROOTFS, OUT/trees/<name>/image and its components.json.
SOURCE_DATE_EPOCH must be set; chunkah reads it.
"""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

# Trees that change on their own schedule, by name. Paths are absolute in the image.
TREES = {"windows-runners": "/usr/lib/tilo/windows-runners"}
TREE_LAYERS = 16


def outside(rootfs: Path, tree: str) -> list[str]:
    """Every path beside the ancestors of `tree`: pruning them leaves the tree and its parents."""
    pruned, parent = [], Path("/")
    for part in Path(tree).relative_to("/").parts:
        for name in sorted(os.listdir(rootfs / parent.relative_to("/"))):
            if name != part:
                pruned.append(str(parent / name))
        parent = parent / part
    return pruned


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
    # /sysroot/: the base's ostree repository, which bootc's composefs backend
    # never deploys. A trailing slash keeps the directory itself.
    main_prune = ["/sysroot/", *(path + "/" for path in trees.values())]
    out.mkdir(parents=True, exist_ok=True)
    chunkah(rootfs, out / "main", max_layers, main_prune)
    (out / "main" / "image").rename(out / "image")
    (out / "main" / "components.json").rename(out / "components.json")
    (out / "main").rmdir()
    for name, path in trees.items():
        chunkah(rootfs, out / "trees" / name, TREE_LAYERS, outside(rootfs, path))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
