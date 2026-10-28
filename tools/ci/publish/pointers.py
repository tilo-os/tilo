#!/usr/bin/env python3
"""Read the pointers the app and desktop releases set, checked before anything uses them.

On t a release check publishes a pointer (t's pipelines document, "Pointers"): `publish-apps` the
`app-collection` pointer to the collections it uploaded, `publish-desktop-payload` the
`desktop-payload` pointer to the OS desktop payloads. They replace the repository variables
`TILO_APP_COLLECTION_*` and `TILO_DESKTOP_PAYLOAD_*` the Forgejo workflows wrote. A check reads one
as `T_POINTER_<NAME>` (`reads_pointers = ["stable:app-collection"]`), or as the pointer file in
the outputs of the check that sets it, when it `needs` that check in the same run.

    pointers.py show NAME [--file FILE]                the pointer, checked, as JSON
    pointers.py get NAME FIELD [--arch ARCH] [--file FILE]
                                                       one field: url and sha256 (of ARCH's
                                                       archive), source, builder

Without --file the value is $T_POINTER_<NAME>. Exit status 3: the pointer is not set (no file, or
the variable unset or empty), or it names nothing for ARCH; 1: it is not a pointer of that name.

What a pointer holds (schema 1; members not named here are ignored, so a publisher can add some):

    app-collection   {"schema": 1, "collection": "default",
                      "x86_64": {"url", "sha256"}, "aarch64": {"url", "sha256"} or null,
                      "builder": the CI builder image the runtime was built in,
                      "source": the commit the collection was built from, or null}
    desktop-payload  {"schema": 1, "x86_64": {"url", "sha256"} or null,
                      "aarch64": {"url", "sha256"} or null, "source": the commit}

Every URL is an immutable generic package of tilo on code.tilo.so; anything else is refused, so a
pointer can't send a build to fetch from elsewhere.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import sys

NAMES = ("app-collection", "desktop-payload")
ARCHES = ("x86_64", "aarch64")
PACKAGE_BASE = "https://code.tilo.so/api/packages/tilo/generic/"
SHA256 = re.compile(r"[0-9a-f]{64}")
COMMIT = re.compile(r"[0-9a-f]{40}")
# A package path: name, version, file, each of the characters Forgejo's generic packages take.
PACKAGE_PATH = re.compile(r"[A-Za-z0-9._+-]+/[A-Za-z0-9._+-]+/[A-Za-z0-9._+-]+")


class PointerError(Exception):
    """The value is not a pointer of the name it was read as."""


class Unset(Exception):
    """The pointer is not set, or names nothing for the architecture asked for."""


def variable(name: str) -> str:
    """The environment variable t gives a check that reads the pointer NAME."""
    return "T_POINTER_" + name.upper().replace("-", "_")


def _entry(name: str, arch: str, value: object) -> dict | None:
    if value is None:
        return None
    if not isinstance(value, dict):
        raise PointerError(f"{name}: {arch} is not an object")
    url, sha256 = value.get("url"), value.get("sha256")
    if not isinstance(url, str) or not url.startswith(PACKAGE_BASE) \
            or not PACKAGE_PATH.fullmatch(url[len(PACKAGE_BASE):]) or "/../" in url or "/./" in url:
        raise PointerError(f"{name}: {arch} names {url!r}, not a generic package of tilo at {PACKAGE_BASE}")
    if not isinstance(sha256, str) or not SHA256.fullmatch(sha256.lower()):
        raise PointerError(f"{name}: {arch} has no SHA-256")
    return {"url": url, "sha256": sha256.lower()}


def _source(name: str, value: object, required: bool) -> str | None:
    if value is None and not required:
        return None
    if not isinstance(value, str) or not COMMIT.fullmatch(value):
        raise PointerError(f"{name}: source {value!r} is not a commit")
    return value


def parse(name: str, text: str) -> dict:
    """The pointer NAME holding TEXT, checked and normalized (lowercase digests)."""
    if name not in NAMES:
        raise PointerError(f"no pointer {name!r}: one of {', '.join(NAMES)}")
    try:
        value = json.loads(text)
    except json.JSONDecodeError as error:
        raise PointerError(f"{name}: not JSON: {error}") from None
    if not isinstance(value, dict) or value.get("schema") != 1:
        raise PointerError(f"{name}: not a schema 1 pointer")
    pointer: dict = {"schema": 1}
    for arch in ARCHES:
        pointer[arch] = _entry(name, arch, value.get(arch))
    if name == "app-collection":
        if value.get("collection") != "default":
            raise PointerError(f"{name}: collection {value.get('collection')!r}, not the default collection")
        if pointer["x86_64"] is None:
            raise PointerError(f"{name}: no x86_64 collection")
        builder = value.get("builder")
        if not isinstance(builder, str) or not builder:
            raise PointerError(f"{name}: no builder")
        pointer.update(collection="default", builder=builder, source=_source(name, value.get("source"), False))
    else:
        if pointer["x86_64"] is None and pointer["aarch64"] is None:
            raise PointerError(f"{name}: no payload for any architecture")
        pointer["source"] = _source(name, value.get("source"), True)
    return pointer


def read(name: str, file: Path | None = None, environ=os.environ) -> dict:
    """The pointer NAME from FILE, else from its variable. Raises Unset when there is none."""
    if file is not None:
        if not file.is_file():
            raise Unset(f"no {name} pointer at {file}")
        text = file.read_text(encoding="utf-8")
    else:
        text = environ.get(variable(name), "")
    if not text.strip():
        raise Unset(f"the {name} pointer is not set")
    return parse(name, text)


def entry(pointer: dict, arch: str) -> dict:
    """What POINTER names for ARCH: {"url", "sha256"}. Raises Unset when it names nothing."""
    if arch not in ARCHES:
        raise PointerError(f"no architecture {arch!r}: one of {', '.join(ARCHES)}")
    if pointer[arch] is None:
        raise Unset(f"the pointer names nothing for {arch}")
    return pointer[arch]


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n", 1)[0])
    commands = parser.add_subparsers(dest="command", required=True)
    show = commands.add_parser("show")
    show.add_argument("name", choices=NAMES)
    show.add_argument("--file", type=Path)
    get = commands.add_parser("get")
    get.add_argument("name", choices=NAMES)
    get.add_argument("field", choices=("url", "sha256", "source", "builder"))
    get.add_argument("--arch", choices=ARCHES)
    get.add_argument("--file", type=Path)
    args = parser.parse_args(argv)
    try:
        pointer = read(args.name, args.file)
        if args.command == "show":
            print(json.dumps(pointer, sort_keys=True))
            return 0
        if args.field in ("url", "sha256"):
            if args.arch is None:
                parser.error(f"{args.field} is per architecture: give --arch")
            print(entry(pointer, args.arch)[args.field])
            return 0
        if args.field not in pointer:
            raise PointerError(f"a {args.name} pointer has no {args.field}")
        value = pointer[args.field]
        if value is None:
            raise Unset(f"the {args.name} pointer names no {args.field}")
        print(value)
        return 0
    except Unset as unset:
        print(f"pointers: {unset}", file=sys.stderr)
        return 3
    except PointerError as error:
        print(f"pointers: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
