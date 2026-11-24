#!/usr/bin/env python3
"""A tilolang package's source catalogue, as its build writes it.

A package's words are its sources': the compiler writes `locales/en-US.ftl` from
them into the package's module, and no copy of it is committed. A tool that
reads a package's words asks the compiler here — `tilolang catalogue <package>`
— with the binary `--tilolang PATH` names, `$TILOLANG`, or the tree's own built
`target/debug/tilolang`.

Paths are relative to the tree's root, as `tilolang` and the checks read them.
"""

from __future__ import annotations

import os
import shutil
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SOURCE_LOCALE = "en-US"
PACKAGE_SOURCE = "app.tl"


class Unread(Exception):
    """The compiler a package's words are read with is not at hand."""


class CannotRead(Unread):
    """A package the compiler refused: it has no words to read until it compiles, which the
    checks that compile a package report."""


def is_package(unit: Path) -> bool:
    """Whether `unit` is a tilolang package: a directory of `.tl` files."""
    return (unit / PACKAGE_SOURCE).is_file()


def packages(root: Path) -> list[Path]:
    """Every tilolang package under `root`, sorted."""
    return sorted(
        path.parent
        for path in root.rglob(PACKAGE_SOURCE)
        if "target" not in path.relative_to(root).parts
        and not any(part.startswith(".") for part in path.relative_to(root).parts)
    )


def writes_catalogues(binary: str) -> bool:
    """Whether `binary` is a `tilolang` new enough to write a package's source catalogue: one built
    before the command existed answers its own usage instead."""
    result = subprocess.run([binary, "catalogue", "--help"], capture_output=True, text=True,
                            check=False)
    return result.returncode == 0


def binary(asked: str | None, root: Path = ROOT) -> str:
    """The compiler that writes a package's source catalogue, refusing when there is none.

    A binary an older build left in a target directory answers no `catalogue`, so it is not one
    this tree can read a package's words with: such a candidate is passed over for the tree's own
    build."""
    target = Path(os.environ.get("CARGO_TARGET_DIR") or root / "target")
    candidates = [asked, os.environ.get("TILOLANG"),
                  str(target / "debug" / "tilolang"),
                  str(target / "release" / "tilolang"),
                  str(root / "target" / "debug" / "tilolang"),
                  shutil.which("tilolang")]
    for candidate in candidates:
        if candidate and Path(candidate).is_file() and os.access(candidate, os.X_OK) \
                and writes_catalogues(candidate):
            return candidate
    # Nothing built yet: this tree's compiler is the one that writes a package's catalogue, and
    # cargo knows whether it is fresh, so a checked tree pays only the fingerprint check.
    cargo = os.environ.get("CARGO") or shutil.which("cargo")
    if cargo and (root / "Cargo.toml").is_file():
        built = subprocess.run(
            [cargo, "build", "--locked", "--quiet", "-p", "tilolang-cli", "--bin", "tilolang"],
            cwd=root, capture_output=True, text=True, check=False)
        built_binary = target / "debug" / "tilolang"
        if built.returncode == 0 and built_binary.is_file() \
                and writes_catalogues(str(built_binary)):
            return str(built_binary)
    raise Unread(
        "no tilolang to write a package's source catalogue with: build one "
        "(`cargo build -p tilolang-cli --bin tilolang`) and name it with --tilolang"
    )


def catalogue(unit: Path, asked: str | None = None, sources: Path | None = None,
              root: Path = ROOT) -> str:
    """The source catalogue `unit`'s sources write.

    `sources` is where the package's sources stand when they are not the checkout's (a revision
    read from Git); the packages it uses are still found beside `unit`."""
    command = [binary(asked, root), "catalogue",
               str(sources if sources is not None else unit)]
    if sources is not None:
        command.extend(["--path", str(unit.parent)])
    result = subprocess.run(command, capture_output=True, text=True, check=False)
    if result.returncode != 0:
        said = (result.stderr or result.stdout).strip()
        where = unit.relative_to(root) if unit.is_relative_to(root) else unit
        raise CannotRead(f"{where}: {said or 'the compiler wrote no source catalogue'}")
    return result.stdout


def take_flag(arguments: list[str]) -> tuple[str | None, list[str]]:
    """`--tilolang PATH` taken off a command line, with the rest of it."""
    asked = None
    rest: list[str] = []
    iterator = iter(arguments)
    for argument in iterator:
        if argument == "--tilolang":
            asked = next(iterator, None)
            if asked is None:
                raise Unread("--tilolang takes the compiler's path")
            continue
        rest.append(argument)
    return asked, rest
