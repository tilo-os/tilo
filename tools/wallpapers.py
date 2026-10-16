#!/usr/bin/env python3
"""Make, publish and stage the wallpapers Tilo ships.

crates/platform/tilo-wallpapers/catalog.toml lists every wallpaper: its
author, licence and source page, the original file (by URL and SHA-256), and
the two files the OS image installs for it, a picture at the laptop panel's
resolution and a thumbnail for the Settings picker. Those files are binaries,
so they stay out of Git (docs/policies.md): they live in immutable artifact
storage, pinned by size and SHA-256, like the font catalog.

  make DIR [--originals DIR]   fetch and verify the originals, then encode the
                               installed files into DIR with ImageMagick
                               (`magick`, built with libwebp). Prints each
                               file's size and digest for the catalog.
  publish DIR                  upload DIR's files to artifact storage; needs
                               TILO_FORGE_USER and TILO_FORGE_TOKEN.
  prepare DEST [--cache DIR]   fetch the pinned files by digest (verifying
                               each) and stage DEST/backgrounds/tilo and the
                               credits, DEST/licenses/tilo-wallpapers/CREDITS.md.
                               --offline, or TILO_WALLPAPER_ASSETS_OFFLINE=1,
                               refuses to fetch what the cache lacks.
  credits                      print the credits document.

Only `make` needs anything beyond the Python 3.11 standard library.
"""
import argparse
import hashlib
import os
import shutil
import subprocess
import sys
import tempfile
import tomllib
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
CATALOG = REPO / "crates/platform/tilo-wallpapers/catalog.toml"
STORE = "https://code.tilo.so/api/packages/tilo/generic/source-evidence"
USER_AGENT = "TiloWallpapers/1.0 (https://code.tilo.so/tilo/tilo)"
# The encoder settings the catalog's files were made with.
PICTURE_QUALITY = 90
THUMBNAIL_QUALITY = 82


def catalog():
    data = tomllib.loads(CATALOG.read_text(encoding="utf-8"))
    if data.get("schema") != 1:
        raise SystemExit(f"{CATALOG}: unsupported schema")
    return data


def assets(entry):
    return [entry["picture"], entry["thumbnail"]]


def artifact_url(asset):
    suffix = Path(asset["file"]).suffix
    return f"{STORE}/{asset['sha256']}/wallpaper-{asset['sha256']}{suffix}"


def sha256(path):
    with open(path, "rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def fetch(url, destination):
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(request, timeout=120) as response, open(destination, "wb") as out:
        shutil.copyfileobj(response, out)


def verified(path, digest, size=None):
    return path.is_file() and (size is None or path.stat().st_size == size) and sha256(path) == digest


def cached(asset, cache, offline):
    """The verified file for `asset` in `cache`, fetched first when missing."""
    path = Path(cache) / asset["sha256"] / asset["file"]
    if path.exists():
        if not verified(path, asset["sha256"], asset["bytes"]):
            raise SystemExit(f"corrupt cache entry {path}: remove it")
        return path
    if offline:
        raise SystemExit(f"{asset['file']} is not in the cache {cache} and fetching is off")
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as partial:
        partial_path = Path(partial.name)
    try:
        fetch(artifact_url(asset), partial_path)
        if not verified(partial_path, asset["sha256"], asset["bytes"]):
            raise SystemExit(f"{artifact_url(asset)} does not match the catalog")
        partial_path.rename(path)
    finally:
        partial_path.unlink(missing_ok=True)
    return path


def credits_text(data):
    lines = [
        "# Tilo wallpapers",
        "",
        "The desktop wallpapers Tilo ships, installed in /usr/share/backgrounds/tilo.",
        "Each is resized and re-encoded from the original named here; nothing else",
        "is changed.",
        "",
    ]
    for entry in data["wallpaper"]:
        lines += [
            f"## {entry['title']}",
            "",
            f"- Files: {entry['picture']['file']}, {entry['thumbnail']['file']}",
            f"- Author: {entry['author']}",
            f"- Licence: {entry['license']} ({entry['license_url']})",
            f"- Source: {entry['source_url']}",
            f"- Original: {entry['original_url']} (SHA-256 {entry['original_sha256']})",
            "",
        ]
    return "\n".join(lines)


def make(args):
    data = catalog()
    out = Path(args.dir)
    out.mkdir(parents=True, exist_ok=True)
    originals = Path(args.originals) if args.originals else out / "originals"
    originals.mkdir(parents=True, exist_ok=True)
    for entry in data["wallpaper"]:
        original = originals / (entry["id"] + Path(entry["original_url"]).suffix.lower())
        if not verified(original, entry["original_sha256"]):
            fetch(entry["original_url"], original)
            if not verified(original, entry["original_sha256"]):
                raise SystemExit(f"{entry['original_url']} does not match the catalog")
        for asset, quality in ((entry["picture"], PICTURE_QUALITY), (entry["thumbnail"], THUMBNAIL_QUALITY)):
            size = f"{asset['width']}x{asset['height']}"
            target = out / asset["file"]
            subprocess.run(
                ["magick", str(original), "-colorspace", "sRGB", "-filter", "Lanczos",
                 "-resize", size + "^", "-gravity", "center", "-extent", size, "-strip",
                 "-define", "webp:method=6", "-quality", str(quality), str(target)],
                check=True,
            )
            matches = "matches" if verified(target, asset["sha256"], asset["bytes"]) else "differs from"
            print(f"{asset['file']}: bytes = {target.stat().st_size}, sha256 = \"{sha256(target)}\" ({matches} the catalog)")


def publish(args):
    user, token = os.environ.get("TILO_FORGE_USER"), os.environ.get("TILO_FORGE_TOKEN")
    if not user or not token:
        raise SystemExit("set TILO_FORGE_USER and TILO_FORGE_TOKEN")
    for entry in catalog()["wallpaper"]:
        for asset in assets(entry):
            path = Path(args.dir) / asset["file"]
            if not verified(path, asset["sha256"], asset["bytes"]):
                raise SystemExit(f"{path} does not match the catalog")
            subprocess.run(
                ["curl", "--fail-with-body", "-sS", "-u", f"{user}:{token}", "-X", "PUT",
                 "--upload-file", str(path), artifact_url(asset)],
                check=False,
            )
            print(artifact_url(asset))


def prepare(args):
    data = catalog()
    destination = Path(args.dest)
    offline = args.offline or os.environ.get("TILO_WALLPAPER_ASSETS_OFFLINE") == "1"
    cache = Path(args.cache or os.environ.get("TILO_WALLPAPER_ASSET_CACHE") or REPO / "target/wallpaper-artifacts")
    backgrounds = destination / "backgrounds/tilo"
    licenses = destination / "licenses/tilo-wallpapers"
    backgrounds.mkdir(parents=True, exist_ok=True)
    licenses.mkdir(parents=True, exist_ok=True)
    for entry in data["wallpaper"]:
        for asset in assets(entry):
            shutil.copyfile(cached(asset, cache, offline), backgrounds / asset["file"])
    (licenses / "CREDITS.md").write_text(credits_text(data) + "\n", encoding="utf-8")
    print(destination)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    make_parser = commands.add_parser("make")
    make_parser.add_argument("dir")
    make_parser.add_argument("--originals")
    publish_parser = commands.add_parser("publish")
    publish_parser.add_argument("dir")
    prepare_parser = commands.add_parser("prepare")
    prepare_parser.add_argument("dest")
    prepare_parser.add_argument("--cache")
    prepare_parser.add_argument("--offline", action="store_true")
    commands.add_parser("credits")
    args = parser.parse_args()
    if args.command == "make":
        make(args)
    elif args.command == "publish":
        publish(args)
    elif args.command == "prepare":
        prepare(args)
    else:
        sys.stdout.write(credits_text(catalog()) + "\n")


if __name__ == "__main__":
    main()
