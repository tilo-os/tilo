"""Shared discovery and slicing of workspace Rust test sources for policy checks."""

from __future__ import annotations

import os
import subprocess
import sys
from collections.abc import Iterator
from pathlib import Path

_POLICY_DIR = str(Path(__file__).resolve().parent)
if _POLICY_DIR not in sys.path:
    sys.path.insert(0, _POLICY_DIR)
import rust_source  # noqa: E402

# Test support: every source of the crate tests share is test code, as the
# files under `tests/` are.
SUPPORT = (Path("tools/test-env/src"),)
SCANNED = ("apps", "crates", "examples", "fuzz", "os", "tools")


def test_code(relative: Path, code: str, *, skip_fixtures: bool = False) -> str:
    """The test-only slices of `code`, blanking production code while keeping newlines.

    When `skip_fixtures` is set, sources under a `fixtures/` directory are
    returned empty: a fixture payload is built and started as its own process
    (often with a bare `rustc`), so it links no test-support crate and holds no
    test thread's process environment.
    """
    if skip_fixtures and any(part == "fixtures" for part in relative.parts[:-1]):
        return ""
    if rust_source.is_test_file(relative) or any(
        support in relative.parents for support in SUPPORT
    ):
        return code
    production = rust_source.blank_test_items(code)
    return "".join(
        c if c != p or c == "\n" else " " for c, p in zip(code, production)
    )


def own_workspace(relative: Path, root: Path) -> bool:
    """Whether `relative` belongs to the repository's Cargo workspace rather
    than to a tool with a workspace and lock of its own."""
    return not any(
        (root / parent / "Cargo.lock").is_file()
        for parent in relative.parents
        if parent != Path(".")
    )


def sources(root: Path) -> Iterator[tuple[Path, str]]:
    """Every Rust source file belonging to the repository's Cargo workspace."""
    names = subprocess.run(
        [
            "git",
            "-C",
            str(root),
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
            "--",
            *SCANNED,
        ],
        stdout=subprocess.PIPE,
        check=True,
    ).stdout
    for name in sorted(set(names.split(b"\0")) - {b""}):
        relative = Path(os.fsdecode(name))
        if relative.suffix != ".rs":
            continue
        if any(part in {"target", "vendor"} for part in relative.parts):
            continue
        if not own_workspace(relative, root):
            continue
        path = root / relative
        if path.is_file():
            yield relative, path.read_text(encoding="utf-8", errors="replace")
