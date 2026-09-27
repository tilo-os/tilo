#!/usr/bin/env python3
"""Fail when a crate's tests/*.rs file is not compiled into any test target.

With `autotests = false` (the single `tests/it.rs` binary layout), Cargo only
builds the declared `[[test]]` targets, so a file under `tests/` that no target
root declares as a module is silently never compiled or run.
"""

from __future__ import annotations

import argparse
import re
import sys
import tomllib
from pathlib import Path

MOD_DECL = re.compile(
    r"^[ \t]*(?:#\[cfg[^\n]*\][ \t]*\n[ \t]*)*"
    r"(?:#\[path[ \t]*=[ \t]*\"(?P<path>[^\"]+)\"\][ \t]*\n?[ \t]*)?"
    r"(?:pub(?:\([^)]*\))?[ \t]+)?mod[ \t]+(?P<name>[A-Za-z_][A-Za-z0-9_]*)[ \t]*;",
    re.MULTILINE,
)
PATH_ATTR = re.compile(r"#\[path[ \t]*=[ \t]*\"(?P<path>[^\"]+)\"\]")
INCLUDE = re.compile(r"include!\([ \t]*\"(?P<path>[^\"]+)\"[ \t]*\)")


def workspace_members(root: Path) -> list[Path]:
    with (root / "Cargo.toml").open("rb") as handle:
        manifest = tomllib.load(handle)
    return [root / member for member in manifest["workspace"]["members"]]


def test_roots(crate: Path, manifest: dict) -> set[Path]:
    tests = crate / "tests"
    roots: set[Path] = set()
    package = manifest.get("package", {})
    if package.get("autotests", True):
        roots.update(tests.glob("*.rs"))
        roots.update(tests.glob("*/main.rs"))
    for target in manifest.get("test", []):
        path = target.get("path") or f"tests/{target['name']}.rs"
        roots.add(crate / path)
    return {root.resolve() for root in roots}


def referenced_by_path(crate: Path) -> set[Path]:
    """Files other sources of the crate pull in with `#[path]` or `include!`."""
    found: set[Path] = set()
    for source in crate.rglob("*.rs"):
        if "target" in source.relative_to(crate).parts:
            continue
        text = source.read_text(encoding="utf-8", errors="replace")
        for pattern in (PATH_ATTR, INCLUDE):
            for match in pattern.finditer(text):
                found.add((source.parent / match["path"]).resolve())
    return found


def declared_modules(root_file: Path) -> set[Path]:
    text = root_file.read_text(encoding="utf-8", errors="replace")
    base = root_file.parent
    found: set[Path] = set()
    for match in MOD_DECL.finditer(text):
        if match["path"]:
            found.add((base / match["path"]).resolve())
        else:
            found.add((base / f"{match['name']}.rs").resolve())
    return found


def orphans(root: Path) -> list[str]:
    errors: list[str] = []
    for crate in workspace_members(root):
        tests = crate / "tests"
        if not tests.is_dir():
            continue
        with (crate / "Cargo.toml").open("rb") as handle:
            manifest = tomllib.load(handle)
        roots = test_roots(crate, manifest)
        reached = set(roots)
        for test_root in roots:
            if test_root.is_file():
                reached |= declared_modules(test_root)
        candidates = sorted(path.resolve() for path in tests.glob("*.rs"))
        if not all(path in reached for path in candidates):
            reached |= referenced_by_path(crate)
        for path in candidates:
            if path not in reached:
                rel = path.relative_to(root.resolve()).as_posix()
                errors.append(
                    f"{rel} is not compiled by any test target; "
                    "declare it in tests/it.rs or delete it"
                )
    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--root", type=Path, default=Path(__file__).resolve().parents[2]
    )
    args = parser.parse_args()
    errors = orphans(args.root)
    for error in errors:
        print(error, file=sys.stderr)
    if errors:
        return 1
    print("integration test modules OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
