#!/usr/bin/env python3
"""Keep what a build's own key signs apart from the bytes it signs.

    signed-records.py runtimes IMAGE OUT     move the runtimes' signed manifests
                                             and publisher keys from IMAGE to OUT
    signed-records.py name-trees STORE       name each app's installed tree by its
                                             payload, not by its package id
    signed-records.py store STORE OUT        move the store's records that name a
                                             package by its id from STORE to OUT

A build that publishes nothing signs the runtime bundles and the app store with
keys it generates and discards. Its signatures, and every name derived from
them, therefore differ in each build even where the signed bytes are the same,
and when they share an OCI layer with those bytes the whole layer is new in
every image: hundreds of megabytes sent again for an unchanged runtime. Here
the signed records go to a tree of their own, which the image copies in a
small layer of its own after the content.

Nothing is verified differently. A runtime bundle's directory is named by its
payload already; its bundle.toml (the file list, the root hash and the
publisher's signatures) is the same file at the same path, only in the next
layer. An installed app is verified by replaying its record (signed root,
manifest, collection) beside its manifest, then every payload file under the
row's payload_root against the signed collection (package-store's
integrity::verify); a row's payload_root may name any tree in the store. Its
tree is named here by the payload root hash its signed root commits to, so a
tree holding the same files has the same name in every build. The image
build runs `tilo-install verify` over the store afterwards.
"""

from __future__ import annotations

import os
import shutil
import sys
import tomllib
from pathlib import Path

TREES = "trees"
# The signed root: magic (8), format (4), manifest hash (32), payload root
# hash (32), ... (tilo-app-package's signed.rs).
PAYLOAD_HASH = slice(44, 76)
SIGNED_ROOT_LEN = 173
RUNTIME_PUBLISHERS = ("runtime-bundle-publisher.hex", "browser-runtime-publisher.hex")


def move(source: Path, root: Path, out: Path) -> None:
    target = out / source.relative_to(root)
    target.parent.mkdir(parents=True, exist_ok=True)
    os.rename(source, target)


def runtimes(image: Path, out: Path) -> None:
    moved = 0
    for manifest in sorted((image / "usr/lib/tilo-runtime").glob("*/bundle.toml")):
        move(manifest, image, out)
        moved += 1
    if not moved:
        raise SystemExit(f"signed-records: no runtime bundle under {image}/usr/lib/tilo-runtime")
    for name in RUNTIME_PUBLISHERS:
        move(image / "usr/share/tilo" / name, image, out)
    print(f"signed-records: {moved} runtime manifests and their publishers moved to {out}")


def rows(store: Path) -> list[dict]:
    with (store / "registry.toml").open("rb") as stream:
        return tomllib.load(stream).get("entry", [])


def name_trees(store: Path) -> None:
    registry = store / "registry.toml"
    text = registry.read_text(encoding="utf-8")
    trees = store / TREES
    named = 0
    for row in rows(store):
        old = Path(row["payload_root"])
        meta = Path(row["manifest_path"]).parent
        if old.parent != store / "pkgs" or meta != old.parent / f"{old.name}.pkg":
            raise SystemExit(f"signed-records: {row['id']} is not installed in {store}/pkgs")
        signed = (meta / "signed-root").read_bytes()
        if len(signed) != SIGNED_ROOT_LEN:
            raise SystemExit(f"signed-records: {meta}/signed-root is not a signed root")
        new = trees / signed[PAYLOAD_HASH].hex()
        if new.exists():
            # The same files under another package: one tree serves both.
            shutil.rmtree(old)
        else:
            trees.mkdir(exist_ok=True)
            os.rename(old, new)
        line = f'payload_root = "{old}"\n'
        if text.count(line) != 1:
            raise SystemExit(f"signed-records: {registry} does not name {old} once")
        text = text.replace(line, f'payload_root = "{new}"\n')
        named += 1
    registry.write_text(text, encoding="utf-8")
    if any(Path(row["payload_root"]).parent != trees for row in rows(store)):
        raise SystemExit(f"signed-records: {registry} still names a tree outside {trees}")
    print(f"signed-records: {named} app trees named by their payload in {trees}")


def store(store: Path, out: Path) -> None:
    # The rows name the store where the image has it; STORE may be a copy.
    records = [store / "registry.toml"]
    for row in rows(store):
        meta = Path(row["manifest_path"]).parent
        records.append(store / meta.relative_to(meta.parents[1]))
    if (store / "bin").exists():
        records.append(store / "bin")
    for record in records:
        move(record, store, out)
    print(f"signed-records: {len(records)} records naming packages by id moved to {out}")


def main(argv: list[str]) -> int:
    if len(argv) == 4 and argv[1] == "runtimes":
        runtimes(Path(argv[2]), Path(argv[3]))
    elif len(argv) == 3 and argv[1] == "name-trees":
        name_trees(Path(argv[2]))
    elif len(argv) == 4 and argv[1] == "store":
        store(Path(argv[2]), Path(argv[3]))
    else:
        print("\n".join(__doc__.strip().splitlines()[2:8]), file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
