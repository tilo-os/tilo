#!/usr/bin/env python3
"""Fail when a crate holds a Rust source none of its targets compiles.

Cargo compiles a crate from the files its targets reach: every target root
(``src/lib.rs``, a bin, a test, a bench, an example, ``build.rs``) and, through
the ``mod`` declarations, ``#[path]`` attributes and ``include!`` macros of
those files, the sources they name. The check reads the directories a crate
compiles from (``src``, ``tests``, ``benches``, ``examples``), not fixture
trees, which the test that copies one compiles itself. A file no path reaches is never built and
never tested, yet it sits in the crate as code: readers take it for live, and a
merge or a copy brings in a second implementation of something that already
works.

A crate that keeps its integration tests as modules of one ``tests/it.rs``
binary also sets ``autotests = false``: with auto-discovery left on, a new
``tests/*.rs`` is a test binary of its own again, linking the whole crate the
way every file used to.
"""

from __future__ import annotations

import argparse
import os
import re
import sys
import tomllib
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from judged import add_staged_argument, judged  # noqa: E402
from rust_source import lex as rust_lex  # noqa: E402

MOD_DECL = re.compile(r"\bmod\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)\s*(?P<end>[;{])|[{}]")
PATH_IN_ATTR = re.compile(r"(?<![A-Za-z0-9_])path[ \t]*=[ \t]*\"")


def workspace_members(root: Path) -> list[Path]:
    with (root / "Cargo.toml").open("rb") as handle:
        manifest = tomllib.load(handle)
    return [root / member for member in manifest["workspace"]["members"]]


def target_roots(crate: Path, manifest: dict) -> set[Path]:
    """The files Cargo compiles a crate from, before any ``mod`` is followed.

    Auto-discovery is on unless the manifest turns it off per kind
    (``autobins`` et al.), and a declared target without a ``path`` uses its
    conventional file. ``build.rs`` is a target of its own.
    """
    package = manifest.get("package", {})
    edition = str(package.get("edition", "2015"))
    roots: set[Path] = set()

    def add(path: str | Path) -> None:
        roots.add(normalise(crate / path))

    lib = manifest.get("lib")
    if lib is None:
        if (crate / "src/lib.rs").is_file():
            add("src/lib.rs")
    else:
        add(lib.get("path") or "src/lib.rs")

    if package.get("autobins", True):
        if (crate / "src/main.rs").is_file():
            add("src/main.rs")
        binaries = crate / "src/bin"
        if binaries.is_dir():
            for path in binaries.glob("*.rs"):
                add(path)
            for child in binaries.iterdir():
                if (child / "main.rs").is_file():
                    add(child / "main.rs")
    for kind, auto, directory in (
        ("bin", "autobins", "src/bin"),
        ("bench", "autobenches", "benches"),
        ("test", "autotests", "tests"),
        ("example", "autoexamples", "examples"),
    ):
        if package.get(auto, True) and (crate / directory).is_dir():
            folder = crate / directory
            for path in folder.glob("*.rs"):
                add(path)
            for child in folder.iterdir():
                if (child / "main.rs").is_file():
                    add(child / "main.rs")
        for target in manifest.get(kind, []):
            name = target["name"]
            if target.get("path"):
                add(target["path"])
            elif kind == "bin" and edition == "2015" and (crate / f"src/{name}.rs").is_file():
                add(f"src/{name}.rs")
            else:
                add(f"src/bin/{name}.rs" if kind == "bin" else f"{kind}s/{name}.rs")

    build = package.get("build", True)
    if build is not False:
        script = build if isinstance(build, str) else "build.rs"
        if (crate / script).is_file():
            add(script)
    return roots


def normalise(path: Path) -> Path:
    """``path`` with its ``..`` and ``.`` components collapsed, without asking the filesystem."""
    return Path(os.path.normpath(str(path)))


def module_directory(source: Path, is_root: bool) -> Path:
    """Where the children a file declares by ``mod name;`` live."""
    if is_root or source.name == "mod.rs":
        return source.parent
    return source.parent / source.stem


def declared(source: Path, module_dir: Path, is_root: bool, crate_root: Path) -> set[Path]:
    """The files a source pulls in.

    A ``mod name;`` names a neighbour by convention, from the directory the
    module owns. A literal path ending in ``.rs`` — in ``#[path = "..."]``, in
    ``include!`` however its arguments are put together, or in a macro that
    declares modules from a list — names the file itself, and is resolved from
    the places that can be its root.
    """
    code, literals = rust_lex(source.read_text(encoding="utf-8", errors="replace"))
    found: set[Path] = set()
    base = normalise(module_dir)
    fallback = normalise(source.parent)
    for literal in literals:
        if not literal.value.endswith(".rs"):
            continue
        for parent in (fallback, base, crate_root):
            found.add(normalise(parent / literal.value.lstrip("/")))
    inline: list[str] = []
    braces: list[bool] = []
    pending = 0
    for match in MOD_DECL.finditer(code):
        if match["name"] is None:
            if match.group() == "{":
                braces.append(False)
            elif braces:
                if braces.pop():
                    inline.pop()
            pending = match.end()
            continue
        # A plain `#[path = "..."]` moves the module; `#[cfg_attr(...,
        # path = "...")]` names one platform's file, and both the file and the
        # conventional neighbour compile somewhere.
        decorated = code[pending : match.start()]
        moved = bool(PATH_IN_ATTR.search(decorated)) and "cfg_attr" not in decorated
        if match["end"] == ";":
            if not moved:
                parent = base.joinpath(*inline) if inline else base
                found.add(normalise(parent / f"{match['name']}.rs"))
                found.add(normalise(parent / match["name"] / "mod.rs"))
                found.add(normalise(fallback / f"{match['name']}.rs"))
                found.add(normalise(fallback / match["name"] / "mod.rs"))
        else:
            inline.append(match["name"])
            braces.append(True)
        pending = match.end()
    return found


def reached_files(roots: set[Path], crate_root: Path) -> set[Path]:
    reached = set(roots)
    pending = [root for root in roots if root.is_file()]
    while pending:
        source = pending.pop()
        is_root = source in roots
        found = declared(source, module_directory(source, is_root), is_root, crate_root)
        for child in found - reached:
            reached.add(child)
            if child.is_file():
                pending.append(child)
    return reached


def sources(crate: Path) -> list[Path]:
    """Every Rust file in the directories a crate compiles from.

    ``src``, ``tests``, ``benches`` and ``examples`` hold the sources a target
    can reach. Build output, nested crates and fixture trees do not: a fixture
    is compiled by the test that copies it and gives it a manifest, not by the
    crate around it.
    """
    found: list[Path] = []
    for directory in ("src", "tests", "benches", "examples"):
        for path in crate.joinpath(directory).glob("**/*.rs"):
            parts = path.relative_to(crate).parts
            if "target" in parts or "fixtures" in parts:
                continue
            if any(
                crate.joinpath(*parts[:depth], "Cargo.toml").is_file()
                for depth in range(1, len(parts))
            ):
                continue
            found.append(path)
    return found


def orphans(root: Path = Path("."), staged: bool = False) -> list[str]:
    """Every Rust source of a workspace crate that no target reaches.

    With ``staged``, only the crates the commit touches are judged: a crate no
    staged file belongs to has the verdict it already had. A staged root
    manifest can change the workspace itself, so it judges every crate.
    """
    root = root.resolve()
    changed = judged(root, staged=True).changed() if staged else None
    whole_workspace = changed is None or Path("Cargo.toml") in changed
    errors: list[str] = []
    for crate in workspace_members(root):
        crate = crate.resolve()
        if not whole_workspace:
            member = crate.relative_to(root)
            if not any(member == path or member in path.parents for path in changed):
                continue
        with (crate / "Cargo.toml").open("rb") as handle:
            manifest = tomllib.load(handle)
        roots = target_roots(crate, manifest)
        tests = crate / "tests"
        if normalise(tests / "it.rs") in roots and manifest.get("package", {}).get("autotests", True):
            rel = (crate / "Cargo.toml").relative_to(root).as_posix()
            errors.append(
                f"{rel}: tests are modules of tests/it.rs; set `autotests = false` so a "
                "new tests/*.rs is never a test binary of its own"
            )
        reached = reached_files(roots, crate)
        for path in sorted(sources(crate)):
            if normalise(path) not in reached:
                rel = normalise(path).relative_to(root).as_posix()
                errors.append(
                    f"{rel}: no target of its crate reaches it; declare it from a "
                    "root through mod, #[path] or include!, or delete it"
                )
    return sorted(errors)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--root", type=Path, default=Path(__file__).resolve().parents[2]
    )
    add_staged_argument(parser)
    args = parser.parse_args(argv)
    errors = orphans(args.root, args.staged)
    for error in errors:
        print(error)
    if errors:
        return 1
    print("module reachability OK", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
