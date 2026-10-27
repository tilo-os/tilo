#!/usr/bin/env python3
"""Resolve immutable test assets into a verified cache; support prewarmed offline runs.

A seed is a read-only cache another step filled, laid out as the cache is
(`<sha256>/<file>`): t's `test-assets` check fills one with every manifest the build
scripts name, and the builds that ship (the OS compile, the app release, the
tilo-vm packs) take it as TILO_TEST_ASSET_SEED, so they never reach the network
for an asset it holds. Each seed entry is verified like any cached one; only an
asset no seed holds is fetched, into the writable cache.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import tempfile
from urllib.request import urlopen

MAX_BYTES = 32 * 1024 * 1024


def manifest(path):
    data = json.loads(Path(path).read_text(encoding="utf-8"))
    if not isinstance(data, dict) or set(data) != {"schema", "assets"} or type(data["schema"]) is not int or data["schema"] != 1:
        raise ValueError("unsupported test asset manifest")
    assets = data["assets"]
    if not isinstance(assets, list) or not assets or len(assets) > 1000:
        raise ValueError("invalid test asset inventory")
    names = set()
    for row in assets:
        if not isinstance(row, dict) or set(row) != {"file", "sha256", "bytes", "url"}:
            raise ValueError("invalid test asset fields")
        name, digest, size = row["file"], row["sha256"], row["bytes"]
        if not isinstance(name, str) or not re.fullmatch(r"[a-z0-9][a-z0-9_.-]*\.(?:png|pdf|epub|docx|odt|wav|mp4)", name):
            raise ValueError("invalid test asset filename")
        if not isinstance(digest, str) or not re.fullmatch(r"[a-f0-9]{64}", digest):
            raise ValueError("invalid test asset SHA-256")
        if type(size) is not int or not 0 < size <= MAX_BYTES:
            raise ValueError("invalid test asset length")
        expected = f"https://code.tilo.so/api/packages/tilo/generic/source-evidence/{digest}/{name}"
        if row["url"] != expected or name in names:
            raise ValueError("unpinned URL or duplicate test asset")
        names.add(name)
    return assets


def verify(path, row):
    if path.stat().st_size != row["bytes"]:
        raise ValueError(f"test asset length mismatch: {path}")
    with path.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    if digest != row["sha256"]:
        raise ValueError(f"test asset SHA-256 mismatch: {path}")


def seeded_asset(row, seeds):
    """The verified copy of `row` in the first seed that holds it; `None` when none does."""
    for seed in seeds:
        path = Path(seed) / row["sha256"] / row["file"]
        if path.exists():
            verify(path, row)
            return path.resolve()
    return None


def cached_asset(row, cache, offline, opener, seeds=()):
    seeded = seeded_asset(row, seeds)
    if seeded is not None:
        return seeded
    path = Path(cache) / row["sha256"] / row["file"]
    if path.exists():
        verify(path, row)
        return path.resolve()
    if offline:
        raise ValueError(f"uncached test asset in offline mode: {row['file']}")
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = None
    try:
        with opener(row["url"], timeout=60) as source:
            with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as dest:
                temporary = Path(dest.name)
                remaining = row["bytes"] + 1
                while remaining:
                    block = source.read(min(65536, remaining))
                    if not block:
                        break
                    dest.write(block)
                    remaining -= len(block)
        verify(temporary, row)
        os.replace(temporary, path)
        verify(path, row)
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)
    return path.resolve()


def resolve(path, cache, offline=False, filename=None, opener=urlopen, seeds=()):
    rows = manifest(path)
    if filename is not None:
        rows = [row for row in rows if row["file"] == filename]
        if not rows:
            raise ValueError(f"test asset not declared: {filename}")
    return [cached_asset(row, cache, offline, opener, seeds) for row in rows]


def seeds_from(value):
    """The seed directories TILO_TEST_ASSET_SEED names, separated as PATH is."""
    return [Path(part) for part in (value or "").split(os.pathsep) if part]


def prune_identical_candidates(path, root):
    """Keep changed/new render candidates; omit bytes already pinned and archived."""
    for row in manifest(path):
        candidate = Path(root) / row["file"]
        if candidate.is_file() and candidate.stat().st_size == row["bytes"]:
            with candidate.open("rb") as stream:
                digest = hashlib.file_digest(stream, "sha256").hexdigest()
            if digest == row["sha256"]:
                candidate.unlink()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("manifest", type=Path, nargs="+")
    parser.add_argument("--cache-root", type=Path, default=Path(os.environ.get(
        "TILO_TEST_ASSET_CACHE", str(Path(__file__).resolve().parents[1] / "target/test-assets"))))
    parser.add_argument("--offline", action="store_true", default=os.environ.get("TILO_TEST_ASSETS_OFFLINE") == "1")
    parser.add_argument("--seed", type=Path, action="append", default=seeds_from(os.environ.get("TILO_TEST_ASSET_SEED")),
                        help="a read-only cache to take verified assets from first (repeatable)")
    parser.add_argument("--file")
    parser.add_argument("--prune-identical-candidates", type=Path)
    args = parser.parse_args()
    try:
        if args.prune_identical_candidates is not None:
            for path in args.manifest:
                prune_identical_candidates(path, args.prune_identical_candidates)
            return
        for manifest_path in args.manifest:
            for path in resolve(manifest_path, args.cache_root, args.offline, args.file, seeds=args.seed):
                print(path)
    except (ValueError, OSError, TypeError, KeyError) as error:
        parser.exit(1, f"Tilo test assets: {error}\n")


if __name__ == "__main__":
    main()
