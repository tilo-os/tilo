#!/usr/bin/env python3
"""Verify and stage Tilo fonts. Requires Python 3.11+; no third-party modules."""
import argparse
import errno
import hashlib
import json
import os
import tempfile
import urllib.request
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
    if (source / "LICENSES/UPSTREAM.md").is_file():
        files.append("LICENSES/UPSTREAM.md")
    for name in files:
        target = dest / name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(checked_path(source, name), target)
    verify(dest, scope)


def link(source, dest, scope, extra=None):
    """Stage `scope` at `dest` as hard links to the files of `source`.

    The OS image ships one catalog's bytes under three names: the signed
    runtime's lib/fonts, /usr/share/fonts/tilo and the font-catalogs/<sha>
    fallback. Hard links within one image layer carry the bytes once. A file
    the scope needs that `source` lacks (an OS-only face beside a product
    catalog) is copied from the verified catalog `extra`. Every file is
    verified at `dest` afterwards, so a link never stands in for other bytes.
    """
    source, dest = Path(source), Path(dest)
    data = manifest(source)
    if extra is not None:
        verify(extra, scope)
    dest.mkdir(parents=True, exist_ok=False)
    files = [f["file"] for f in selected(data, scope) + data["license"]]
    files += ["manifest.toml", "PROVENANCE.md"]
    if (source / "LICENSES/UPSTREAM.md").is_file():
        files.append("LICENSES/UPSTREAM.md")
    for name in files:
        target = dest / name
        target.parent.mkdir(parents=True, exist_ok=True)
        if (source / name).is_file():
            os.link(checked_path(source, name), target)
        elif extra is not None:
            shutil.copyfile(checked_path(Path(extra), name), target)
        else:
            raise ValueError(f"missing regular file: {source / name}")
    verify(dest, scope)


def artifact_entries(root):
    """Require artifact pins to describe the complete reviewed font catalog."""
    data = manifest(root)
    inventory = json.loads((root / "artifacts.json").read_text())
    if set(inventory) != {"schema", "assets"} or type(inventory["schema"]) is not int or inventory["schema"] != 1:
        raise ValueError("unsupported font artifact inventory")
    expected = {entry["file"]: entry for entry in data["font"]}
    actual = {}
    for row in inventory["assets"]:
        if not isinstance(row, dict) or set(row) != {"file", "bytes", "sha256", "url"}:
            raise ValueError("invalid font artifact row")
        name = row["file"]
        if not isinstance(name, str) or name not in expected or name in actual:
            raise ValueError("unknown or duplicate font artifact")
        pin = expected[name]
        if type(row["bytes"]) is not int or not 0 < row["bytes"] <= 64 * 1024**2:
            raise ValueError("invalid font artifact size")
        if row["bytes"] != pin["bytes"] or row["sha256"] != pin["sha256"]:
            raise ValueError(f"font artifact differs from reviewed manifest: {name}")
        url = ("https://code.tilo.so/api/packages/tilo/generic/source-evidence/"
               + pin["sha256"] + "/" + ("font-" + pin["sha256"] + Path(name).suffix))
        if row["url"] != url:
            raise ValueError("font artifact URL differs from immutable pin")
        actual[name] = row
    if set(actual) != set(expected):
        raise ValueError("font artifact inventory is incomplete")
    return data, actual


def verify_artifact(path, entry):
    if path.is_symlink() or not path.is_file() or path.stat().st_size != entry["bytes"]:
        raise ValueError(f"invalid cached font size or file type: {path}")
    with path.open("rb") as stream:
        if hashlib.file_digest(stream, "sha256").hexdigest() != entry["sha256"]:
            raise ValueError(f"SHA-256 mismatch: {path}")


def cached_font(entry, cache, offline):
    cache = Path(cache)
    path = cache / entry["sha256"] / entry["file"]
    if path.exists() or path.is_symlink():
        verify_artifact(path, entry)
        return path
    supplied = os.environ.get("TILO_FONT_CATALOG_ROOT")
    if supplied:
        provided = Path(supplied) / entry["file"]
        verify_artifact(provided, entry)
        return provided
    if offline:
        raise ValueError(f"font artifact unavailable offline: {entry['file']}")
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as stream:
            temporary = Path(stream.name)
            with urllib.request.urlopen(entry["url"], timeout=60) as response:
                if response.geturl() != entry["url"]:
                    raise ValueError("font artifact redirect is forbidden")
                left = entry["bytes"] + 1
                while left:
                    block = response.read(min(left, 65536))
                    if not block:
                        break
                    stream.write(block)
                    left -= len(block)
        verify_artifact(temporary, entry)
        os.replace(temporary, path)
        verify_artifact(path, entry)
        return path
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def prepare(source, dest, scope, cache, offline):
    """Materialize exact fonts and license notices into a new build directory."""
    source, dest = Path(source), Path(dest)
    data, entries = artifact_entries(source)
    if dest.exists() or dest.is_symlink():
        raise ValueError(f"font destination already exists: {dest}")
    dest.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".tilo-fonts-", dir=dest.parent) as temporary:
        root = Path(temporary) / "catalog"
        root.mkdir()
        for entry in selected(data, scope):
            shutil.copyfile(cached_font(entries[entry["file"]], cache, offline), root / entry["file"])
        notices = [row["file"] for row in data["license"]] + ["manifest.toml", "PROVENANCE.md"]
        if (source / "LICENSES/UPSTREAM.md").is_file():
            notices.append("LICENSES/UPSTREAM.md")
        for name in notices:
            target = root / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(checked_path(source, name), target)
        verify(root, scope)
        root.rename(dest)


def verify_prepared(root, source, scope):
    verify(root, scope)
    for name in ("PROVENANCE.md", "LICENSES/UPSTREAM.md"):
        expected = source / name
        if expected.is_file() and checked_path(root, name).read_bytes() != expected.read_bytes():
            raise ValueError(f"prepared font provenance differs: {name}")



def default_cache():
    """The artifact cache: TILO_FONT_ASSET_CACHE, else font-artifacts in the cargo target directory."""
    if cache := os.environ.get("TILO_FONT_ASSET_CACHE"):
        return Path(cache)
    target = os.environ.get("CARGO_TARGET_DIR")
    return Path(target) / "font-artifacts" if target else AUTHORITATIVE.parents[5] / "target/font-artifacts"

def prepared_root(source, scope, cache=None, offline=None):
    """Return a verified materialized catalog, without changing the source."""
    source = Path(source)
    supplied = os.environ.get("TILO_FONT_CATALOG_ROOT")
    if supplied:
        root = Path(supplied).resolve(strict=True)
        verify_prepared(root, source, scope)
        return root
    if not (source / "artifacts.json").is_file():
        verify(source, scope)
        return source
    artifact_entries(source)
    cache = Path(cache or default_cache())
    if offline is None:
        offline = os.environ.get("TILO_FONT_ASSETS_OFFLINE") == "1"
    identity = hashlib.sha256()
    for name in ("manifest.toml", "PROVENANCE.md", "LICENSES/UPSTREAM.md"):
        path = source / name
        if path.is_file():
            identity.update(name.encode())
            identity.update(path.read_bytes())
    dest = cache / "catalogs" / (identity.hexdigest() + "-" + scope)
    if dest.exists() or dest.is_symlink():
        if dest.is_symlink():
            raise ValueError("prepared font catalog symlink is forbidden")
        verify_prepared(dest, source, scope)
    else:
        try:
            prepare(source, dest, scope, cache, offline)
        except ValueError as error:
            if str(error) != f"font destination already exists: {dest}":
                raise
            if dest.is_symlink():
                raise ValueError("prepared font catalog symlink is forbidden") from error
            verify_prepared(dest, source, scope)
        except OSError as error:
            if error.errno not in (errno.EEXIST, errno.ENOTEMPTY) and not isinstance(error, FileExistsError):
                raise
            if dest.is_symlink():
                raise ValueError("prepared font catalog symlink is forbidden") from error
            verify_prepared(dest, source, scope)
    verify_prepared(dest, source, scope)
    return dest.resolve(strict=True)

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
    linked = commands.add_parser("link", help="stage a catalog as hard links to another one's files")
    linked.add_argument("source", type=Path)
    linked.add_argument("dest", type=Path)
    linked.add_argument("--scope", choices=list(SCOPES), required=True)
    linked.add_argument("--extra", type=Path, help="a verified catalog for files the source lacks")
    prepared = commands.add_parser("prepare", help="materialize pinned artifacts and notices for an offline build")
    prepared.add_argument("source", type=Path)
    prepared.add_argument("dest", type=Path)
    prepared.add_argument("--scope", choices=list(SCOPES), required=True)
    prepared.add_argument("--cache", type=Path, default=default_cache())
    prepared.add_argument("--offline", action="store_true", default=os.environ.get("TILO_FONT_ASSETS_OFFLINE") == "1")
    locate = commands.add_parser("root", help="print a verified catalog root for build consumers")
    locate.add_argument("source", type=Path)
    locate.add_argument("--scope", choices=list(SCOPES), required=True)
    locate.add_argument("--cache", type=Path)
    locate.add_argument("--offline", action="store_true", default=os.environ.get("TILO_FONT_ASSETS_OFFLINE") == "1")
    args = parser.parse_args()
    try:
        if args.command == "verify":
            print(verify(args.root, args.scope))
        elif args.command == "stage":
            stage(args.source, args.dest, args.scope)
        elif args.command == "link":
            link(args.source, args.dest, args.scope, args.extra)
        elif args.command == "prepare":
            prepare(args.source, args.dest, args.scope, args.cache, args.offline)
        else:
            print(prepared_root(args.source, args.scope, args.cache, args.offline))
    except (ValueError, OSError) as error:
        parser.exit(1, f"Tilo font catalog: {error}\n")

if __name__ == "__main__":
    main()
