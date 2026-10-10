#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Prepare pinned GTK3 history sources privately; never fetch, build or install."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tarfile

HERE = Path(__file__).resolve().parent


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def require_hash(path, expected):
    if digest(path) != expected:
        raise ValueError(f"Pinned source hash mismatch: {path.name}")


def prepare(archive, history, out):
    lock = json.loads((HERE / "source-lock.json").read_text())
    require_hash(archive, lock["gtk3"]["archive_sha256"])
    if archive.stat().st_size != lock["gtk3"]["archive_bytes"]:
        raise ValueError("GTK3 source archive size mismatch")
    for name, expected in lock["history"]["files"].items():
        require_hash(history / name, expected)
    for name, expected in lock["patches"].items():
        require_hash(HERE / name, expected)
    # A fresh private directory is the entire write boundary. No in-place patching.
    out.mkdir(mode=0o700, parents=False, exist_ok=False)
    with tarfile.open(archive, "r:xz") as source:
        members = source.getmembers()
        for member in members:
            parts = Path(member.name).parts
            if not parts or parts[0] != "gtk-3.24.52" or ".." in parts:
                raise ValueError("Source archive member outside pinned root")
        source.extractall(out, filter="data")
    tree = out / "gtk-3.24.52"
    for name, expected in lock["base_files"].items():
        require_hash(tree / name, expected)
    for name in lock["history"]["files"]:
        destination = tree / name
        if destination.exists():
            raise ValueError("History source would overwrite a GTK3 source")
        shutil.copyfile(history / name, destination)
    for name in lock["patches"]:
        subprocess.run(["git", "apply", "--check", str(HERE / name)], cwd=tree, check=True)
        subprocess.run(["git", "apply", str(HERE / name)], cwd=tree, check=True)
    receipt = {
        "source_lock_sha256": digest(HERE / "source-lock.json"),
        "sources": lock,
        "prepared_files": {
            name: digest(tree / name)
            for name in [*lock["base_files"], *lock["history"]["files"]]
        },
        "source_tree": {
            str(path.relative_to(tree)): ({"symlink": str(path.readlink())}
                if path.is_symlink() else {"sha256": digest(path)})
            for path in sorted(tree.rglob("*"))
            if path.is_file() or path.is_symlink()
        },
        "compiled": False,
        "installed": False,
        "production_ready": False,
    }
    (out / "preparation-receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print(tree)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gtk3-archive", type=Path, required=True)
    parser.add_argument("--history-source", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    prepare(args.gtk3_archive.resolve(), args.history_source.resolve(), args.out.resolve())


if __name__ == "__main__":
    main()
