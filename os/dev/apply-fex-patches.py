#!/usr/bin/env python3
"""Apply the reviewed Tilo patch series to a disposable, clean FEX checkout."""

import argparse
import hashlib
import json
import os
import tempfile
from pathlib import Path
import re
import subprocess


def patch_record(directory):
    directory = Path(directory)
    manifest = json.loads((directory / "series.json").read_text())
    if manifest.get("schema_version") != 1 or not re.fullmatch(
            r"[0-9a-f]{40}", manifest.get("base_revision", "")):
        raise ValueError("invalid FEX patch series identity")
    patches = manifest.get("patches")
    if not isinstance(patches, list) or not patches:
        raise ValueError("FEX patch series must not be empty")
    seen = set()
    for patch in patches:
        name, digest = patch.get("file", ""), patch.get("sha256", "")
        if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]*\.patch", name) or name in seen:
            raise ValueError("invalid or duplicate FEX patch filename")
        seen.add(name)
        path = directory / name
        if path.is_symlink() or not path.is_file():
            raise ValueError("FEX patch must be a regular file")
        if not re.fullmatch(r"[0-9a-f]{64}", digest) or hashlib.sha256(path.read_bytes()).hexdigest() != digest:
            raise ValueError("FEX patch checksum mismatch")
    return {"schema_version": 1, "base_revision": manifest["base_revision"],
            "patches": [{"file": p["file"], "sha256": p["sha256"]} for p in patches]}


def apply(source, directory):
    source, directory = Path(source).resolve(), Path(directory).resolve()
    record = patch_record(directory)

    def git(*args, env=None):
        return subprocess.check_output(["git", "-C", str(source), *args], text=True, env=env).strip()

    if git("rev-parse", "HEAD") != record["base_revision"]:
        raise ValueError("FEX patch base differs from checkout")
    if git("status", "--porcelain", "--untracked-files=all"):
        raise ValueError("FEX patch input must be a clean disposable checkout")
    # Apply in order to a private index: later patches may depend on earlier
    # ones. Only the complete result may mutate the caller's checkout/index.
    with tempfile.TemporaryDirectory(prefix="fex-patch-check-") as temporary:
        temporary = Path(temporary)
        env = dict(os.environ, GIT_INDEX_FILE=str(temporary / "index"))
        git("read-tree", "HEAD", env=env)
        for patch in record["patches"]:
            git("apply", "--cached", "--whitespace=error-all",
                str(directory / patch["file"]), env=env)
        tree = git("write-tree", env=env)
        combined = subprocess.check_output([
            "git", "-C", str(source), "diff", "--binary", "HEAD", tree, "--"])
        if combined:
            path = temporary / "combined.patch"
            path.write_bytes(combined)
            git("apply", "--index", "--whitespace=error-all", str(path))
    return record


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("patches", type=Path)
    args = parser.parse_args()
    try:
        print(json.dumps(apply(args.source, args.patches), indent=2, sort_keys=True))
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"FEX patch preparation failed: {error}\n")


if __name__ == "__main__":
    main()
