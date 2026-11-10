#!/usr/bin/env python3
"""Verify and stage Tilo fonts. Requires Python 3.11+; no third-party modules."""
import argparse
import contextlib
import errno
import hashlib
import http.client
import json
import os
import random
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path
import shutil
import tomllib

try:
    import fcntl
except ImportError:  # Windows: a cache there is no other machine's.
    fcntl = None

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
# What fontdb reads of each file, recorded by `tilo-runtime-bundle font-index`
# when a catalog is laid out for a runtime (tilo_font_catalog::faces).
FACE_INDEX = "face-index.json"

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
    # The faces the build recorded for these files (tilo-runtime-bundle
    # font-index) travel with them: every copy of the catalog is read
    # without parsing its fonts.
    if (source / FACE_INDEX).is_file():
        files.append(FACE_INDEX)
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
    # The faces the build recorded for these files (tilo-runtime-bundle
    # font-index) travel with them: every copy of the catalog is read
    # without parsing its fonts.
    if (source / FACE_INDEX).is_file():
        files.append(FACE_INDEX)
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


# The CI builder image's read-only copy of every pinned font (os/Containerfile.builder), laid
# out as an artifact cache by `seed`. A job in that image reads its fonts here instead of
# fetching 56 MB from the release store, so it sends nothing over the network for them.
IMAGE_SEED = Path("/usr/share/tilo/font-artifacts")

# How a fetch answers a release store that is busy. Every job on a build node fetches from the
# same host, so a burst of them can meet its rate limit (429) or a 5xx while it recovers; those
# are waited out rather than failing a job whose fonts are perfectly available. A 404 is a pin
# that is not there, which another attempt cannot fix.
RETRYABLE = frozenset({408, 425, 429, 500, 502, 503, 504})
ATTEMPTS = 5
FIRST_WAIT_SECONDS = 1.0
LONGEST_WAIT_SECONDS = 30.0


class Redirected(Exception):
    """The store answered with another address, which a pin forbids."""


def wait_after(error, attempt):
    """Seconds to wait before trying again, or None when another attempt cannot answer."""
    if isinstance(error, urllib.error.HTTPError):
        if error.code not in RETRYABLE:
            return None
        # The store's own answer for how long it is busy wins over the backoff, bounded so a
        # mistyped header cannot hang a job.
        header = (error.headers or {}).get("Retry-After", "")
        if header.isdigit() and 0 < int(header) <= LONGEST_WAIT_SECONDS:
            return float(header)
    elif not isinstance(error, (urllib.error.URLError, TimeoutError, ConnectionError,
                                http.client.HTTPException)):
        # Anything else is the answer itself: a body that does not verify against its pin is not
        # what a second attempt at the same address is for.
        return None
    return min(FIRST_WAIT_SECONDS * 2 ** (attempt - 1), LONGEST_WAIT_SECONDS) \
        * random.uniform(0.8, 1.2)


def open_url(url):
    """The release store's answer for one pinned font's URL."""
    return urllib.request.urlopen(url, timeout=60)


def fetch(entry, path):
    """Fetch one pinned font into `path`, waiting out a store that is busy.

    `path`'s directory must exist. The bytes land under a temporary name and are verified before
    they take the pinned name, so a job that reads the file never reads a partial or altered one.
    """
    attempt = 1
    while True:
        temporary = None
        try:
            with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as stream:
                temporary = Path(stream.name)
                with open_url(entry["url"]) as response:
                    if response.geturl() != entry["url"]:
                        raise Redirected("font artifact redirect is forbidden")
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
            return
        except Redirected:
            raise ValueError("font artifact redirect is forbidden")
        except (urllib.error.URLError, TimeoutError, OSError, ValueError) as error:
            wait = wait_after(error, attempt)
            if wait is None or attempt == ATTEMPTS:
                raise ValueError(
                    f"could not fetch {entry['file']} from {entry['url']} "
                    f"in {attempt} attempt{'s' if attempt != 1 else ''}: {error}") from error
            print(f"Tilo font catalog: fetching {entry['file']}: {error}; "
                  f"attempt {attempt} of {ATTEMPTS}, again in {wait:.1f}s", file=sys.stderr)
            time.sleep(wait)
            attempt += 1
        finally:
            if temporary is not None:
                temporary.unlink(missing_ok=True)


@contextlib.contextmanager
def locked(cache):
    """Hold the cache's lock, so one job fetches the pinned fonts and publishes the catalog while
    the others wait for it and verify what it left.

    Jobs sharing a build directory share the artifact cache in it (TILO_FONT_ASSET_CACHE). Without
    the lock each of them fetches the whole 56 MB catalog at once, and the store rate-limits the
    node for it. The lock is held on the cache directory itself, so nothing is written beside the
    catalogs it names; a platform without flock, or a file system whose locks fail, is used
    unserialized, as it was before.
    """
    cache = Path(cache)
    if fcntl is None:
        yield
        return
    cache.mkdir(parents=True, exist_ok=True)
    descriptor = os.open(cache, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
    try:
        try:
            fcntl.flock(descriptor, fcntl.LOCK_EX)
        except OSError as error:
            # Said, not swallowed: a cache whose lock is unavailable is one every job fetches
            # into at once, which is what the lock exists to prevent.
            print(f"Tilo font catalog: cannot lock {cache} ({error}); "
                  "fetching without the other jobs' turn", file=sys.stderr)
            yield
            return
        try:
            yield
        finally:
            fcntl.flock(descriptor, fcntl.LOCK_UN)
    finally:
        os.close(descriptor)


def cached_font(entry, cache, offline):
    seeded = IMAGE_SEED / entry["sha256"] / entry["file"]
    if seeded.exists() or seeded.is_symlink():
        verify_artifact(seeded, entry)
        return seeded
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
    fetch(entry, path)
    return path


def prepare(source, dest, scope, cache, offline):
    """Materialize exact fonts and license notices into a new build directory.

    The jobs that share a cache take turns: the first fetches what is missing and publishes a
    verified catalog, and the ones that waited find it there. Whoever comes second raises the
    destination-exists error, and its caller verifies the catalog it found.
    """
    source, dest = Path(source), Path(dest)
    with locked(cache):
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


def seed(source, dest, offline):
    """Lay out every pinned font of `source` at dest/<sha256>/<file>, verified and nothing else:
    the copy the builder image carries at IMAGE_SEED. Fonts already in a seed or the cache are
    copied rather than fetched again."""
    _, entries = artifact_entries(Path(source))
    dest = Path(dest)
    expected = set()
    for entry in entries.values():
        target = dest / entry["sha256"] / entry["file"]
        expected.add(target.relative_to(dest))
        found = cached_font(entry, dest, offline)
        if found != target:
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(found, target)
            verify_artifact(target, entry)
    extra = sorted(str(path.relative_to(dest)) for path in dest.rglob("*")
                   if not path.is_dir() and path.relative_to(dest) not in expected)
    if extra:
        raise ValueError(f"font seed holds files no pin names: {', '.join(extra)}")
    return len(expected)


def verify_prepared(root, source, scope):
    # The catalog's manifest must be the source's, so its fonts are checked against the hashes
    # the source pins, never against ones written beside them in a shared cache.
    for name in ("manifest.toml", "PROVENANCE.md", "LICENSES/UPSTREAM.md"):
        expected = source / name
        if expected.is_file() and checked_path(root, name).read_bytes() != expected.read_bytes():
            raise ValueError(f"prepared font provenance differs: {name}")
    verify(root, scope)



def default_cache():
    """The artifact cache: TILO_FONT_ASSET_CACHE, else font-artifacts in the machine's tilo test
    cache (TILO_TEST_CACHE_DIR, which tools/test-env.sh sets, or ~/.cache/tilo/test-env), which
    outlives build directories: a fresh one would otherwise download and prepare the catalog again
    in every build of the fonts crate, on the critical path of every UI crate. Entries are named
    by their SHA-256 and the catalog by its inputs' digest, and both are verified when read, so a
    shared cache can't serve a stale or altered font. Without a home, the cargo target directory."""
    if cache := os.environ.get("TILO_FONT_ASSET_CACHE"):
        return Path(cache)
    if tests := os.environ.get("TILO_TEST_CACHE_DIR"):
        return Path(tests) / "font-artifacts"
    # A home that isn't a directory can't hold one: a bootc image's /root links to /var/roothome,
    # which exists only once the system boots, so the OS image's builder has none.
    home = os.environ.get("XDG_CACHE_HOME") or (
        os.path.isdir(os.environ.get("HOME", "")) and os.path.join(os.environ["HOME"], ".cache"))
    if home:
        return Path(home) / "tilo/test-env/font-artifacts"
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
            # Another build published the catalog first; any other error (a cache directory that
            # can't be made, for one) is the answer, not a catalog to verify.
            if error.errno not in (errno.EEXIST, errno.ENOTEMPTY) or not (dest.exists() or dest.is_symlink()):
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
    seeded = commands.add_parser("seed", help="lay out every pinned font by digest, as the builder image carries them")
    seeded.add_argument("source", type=Path)
    seeded.add_argument("dest", type=Path)
    seeded.add_argument("--offline", action="store_true", default=os.environ.get("TILO_FONT_ASSETS_OFFLINE") == "1")
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
        elif args.command == "seed":
            print(seed(args.source, args.dest, args.offline))
        else:
            print(prepared_root(args.source, args.scope, args.cache, args.offline))
    except (ValueError, OSError) as error:
        parser.exit(1, f"Tilo font catalog: {error}\n")

if __name__ == "__main__":
    main()
