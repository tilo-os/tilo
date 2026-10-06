#!/usr/bin/env python3
"""Verify and stage Tilo fonts. Requires Python 3.11+; no third-party modules."""
import argparse
import hashlib
from pathlib import Path
import shutil
import tomllib

# Licences the catalog admits: SIL OFL 1.1; the GUST Font License, which is
# LPPL 1.3c with a naming request; and GPL 3 or later with the font exception
# (plus New Computer Modern's distribution exception) for NewCM10-Regular.
LICENSES = {"OFL-1.1", "LPPL-1.3c", "GPL-3.0-or-later WITH Font-exception-2.0"}
# What the SDK's standalone catalog, shipped next to third-party app builds,
# may carry: fonts any app can bundle.
STANDALONE_LICENSES = {"OFL-1.1", "LPPL-1.3c"}

# The manifest scopes each staged catalog carries, its file count, and the
# manifest field that totals its bytes. `product` is the SDK's standalone
# catalog; `os` is what the Tilo OS image installs; `fixtures` is the
# checked-in test catalog.
SCOPES = {
    "product": ({"product"}, 35, "product_bytes"),
    "os": ({"product", "os"}, 36, "os_bytes"),
    "fixtures": ({"product", "os", "fixture"}, 37, "fixture_bytes"),
}

AUTHORITATIVE = Path(__file__).resolve().parents[1] / "crates/sdk/ui-core/assets/fonts/manifest.toml"

def manifest(root):
    path = root / "manifest.toml"
    if not path.is_file():
        raise ValueError(f"missing manifest: {path}")
    raw = path.read_bytes()
    if raw != AUTHORITATIVE.read_bytes():
        raise ValueError(f"manifest differs from this Tilo build: {path}")
    result = tomllib.loads(raw.decode())
    if result["schema"] != 1:
        raise ValueError(f"unsupported schema: {path}")
    return result

def selected(data, scope):
    return [f for f in data["font"] if f["scope"] in SCOPES[scope][0]]

def checked_path(root, name):
    relative = Path(name)
    if relative.is_absolute() or ".." in relative.parts:
        raise ValueError(f"invalid manifest path: {name}")
    path = root / relative
    if not path.is_file():
        raise ValueError(f"missing regular file: {path}")
    return path

def verify(root, scope):
    root = Path(root)
    data = manifest(root)
    fonts = selected(data, scope)
    for entry in fonts + data["license"]:
        path = checked_path(root, entry["file"])
        if "bytes" in entry and path.stat().st_size != entry["bytes"]:
            raise ValueError(f"size mismatch: {path}")
        with path.open("rb") as stream:
            actual = hashlib.file_digest(stream, "sha256").hexdigest()
        if actual != entry["sha256"]:
            raise ValueError(f"SHA-256 mismatch: {path}")
    count, total = len(fonts), sum(f["bytes"] for f in fonts)
    _, files, field = SCOPES[scope]
    expected = (files, data[field])
    if (count, total) != expected:
        raise ValueError(f"invalid catalog payload: {(count, total)} != {expected}")
    licensed = {entry["file"] for entry in data["license"]}
    admitted = STANDALONE_LICENSES if scope == "product" else LICENSES
    for entry in fonts:
        if entry["license_spdx"] not in admitted or not set(entry["license_files"]) <= licensed:
            raise ValueError(f"invalid license mapping: {entry['file']}")
    return count, total

def stage(source, dest, scope):
    source, dest = Path(source), Path(dest)
    verify(source, scope)
    data = manifest(source)
    dest.mkdir(parents=True, exist_ok=False)
    files = [f["file"] for f in selected(data, scope) + data["license"]]
    files += ["manifest.toml", "PROVENANCE.md"]
    for name in files:
        target = dest / name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(checked_path(source, name), target)
    verify(dest, scope)

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    check = commands.add_parser("verify")
    check.add_argument("root", type=Path)
    check.add_argument("--scope", choices=list(SCOPES), required=True)
    copy = commands.add_parser("stage")
    copy.add_argument("source", type=Path)
    copy.add_argument("dest", type=Path)
    copy.add_argument("--scope", choices=list(SCOPES), required=True)
    args = parser.parse_args()
    try:
        if args.command == "verify":
            print(verify(args.root, args.scope))
        else:
            stage(args.source, args.dest, args.scope)
    except (ValueError, OSError) as error:
        parser.exit(1, f"Tilo font catalog: {error}\n")

if __name__ == "__main__":
    main()
