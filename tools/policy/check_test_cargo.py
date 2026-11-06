#!/usr/bin/env python3
"""A test starts only programs its run built; it never runs Cargo itself.

A test run is driven by Cargo, into one target directory. A test that starts
`cargo` inside that run asks for a second build while the run is going on: a
second workspace resolution, and a compile of everything the requested binary
depends on into the same target directory, with features unified another way
than the run's own build unified them, so crates the run has just built are
built again. That near doubles the run's disk and wall time, once per test
process that asks, on every shard that has such a test.

So a test gets the binaries it starts from its run: the package's own binaries
through `tilo_test_env::package_binary!` (Cargo builds them for the package's
tests), and another package's through `tilo_test_env::sibling_binary`, which
answers with what the run's harness built before the first test started
(`tools/build-sibling-binaries.sh`, a nextest setup script in
`.config/nextest.toml`).

This check finds, in test code (files under `tests/` or `benches/`, the
sources of `tilo-test-env`, and `#[cfg(test)]`/`#[test]` items elsewhere), a
command built to run `cargo` — `Command::new("cargo")`, `Command::new($CARGO)`,
`program: cargo_program()`, and the like — whose enclosing item starts a
process. A command a test only fills in, or a plan a test hands to code under
test, is not a Cargo run and is not reported.

A tool with a Cargo workspace and lock file of its own (tools/web/wasm-split)
is skipped: it does not build this tree.
"""
from __future__ import annotations

import os
import re
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import rust_source  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
# Test support: every source of the crate tests share is test code, as the
# files under `tests/` are.
SUPPORT = (Path("crates/dev/test-env/src"),)
SCANNED = ("apps", "crates", "examples", "fuzz", "os", "tools")
# Where a command's program is named: `Command::new(PROGRAM)`,
# `Command::from(PROGRAM)`, a `program:` field of a command, `.program(PROGRAM)`.
PROGRAM = re.compile(
    r"(?:\bCommand\s*::\s*(?:new|from)\s*\(|\.\s*program\s*\(|(?<![\w.])program\s*:)"
)
STARTS = re.compile(r"\.\s*(?:spawn|output|status)(?:_locked)?\s*\(")
CARGO = re.compile(r"cargo", re.IGNORECASE)


def test_code(relative: Path, code: str) -> str:
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


def sources(root: Path):
    names = subprocess.run(
        ["git", "-C", str(root), "ls-files", "--cached", "--others", "--exclude-standard", "-z", "--", *SCANNED],
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


def program_at(code: str, start: int) -> str:
    """The program expression a command is built with, from `start` to the
    `)` `,` or newline that ends it at depth 0."""
    depth = 0
    for index in range(start, len(code)):
        char = code[index]
        if char in "([{":
            depth += 1
        elif char in ")]}":
            if depth == 0:
                return code[start:index]
            depth -= 1
        elif depth == 0 and char in ",\n":
            return code[start:index]
    return code[start:]


FUNCTION = re.compile(r"\bfn\b")


def enclosing_item(code: str, offset: int) -> str:
    """The function around `offset`: the innermost one whose extent contains
    it, else the file up to there."""
    innermost = None
    for keyword in FUNCTION.finditer(code, 0, offset):
        item_start, item_end = rust_source.item_extent(code, keyword.start())
        if item_start <= offset <= item_end and (innermost is None or item_start >= innermost[0]):
            innermost = (item_start, item_end)
    return code[innermost[0] : innermost[1]] if innermost else code[:offset]


def names_cargo(expression: str, code: str) -> bool:
    """Whether the program expression names Cargo: `"cargo"`, `$CARGO`,
    `cargo_program()`, or a variable bound to one of those."""
    if CARGO.search(expression):
        return True
    name = re.match(r"\s*&?\s*([A-Za-z_]\w*)\s*$", expression)
    if not name:
        return False
    binding = re.search(
        rf"\blet\s+(?:mut\s+)?{re.escape(name.group(1))}\s*(?::[^=]+)?=\s*([^;]+);", code
    )
    return bool(binding and CARGO.search(binding.group(1)))


def audit(root: Path = ROOT, files=None) -> list[tuple[str, int, str]]:
    listed = files if files is not None else sources(root)
    found = []
    for relative, text in listed:
        if "cargo" not in text.lower() or not own_workspace(relative, root):
            continue
        code, literals = rust_source.lex(text)
        values = {literal.start: literal.value for literal in literals}
        tests = test_code(relative, code)
        for program in PROGRAM.finditer(tests):
            expression = program_at(tests, program.end())
            end = program.end() + len(expression)
            # `code` has its literals blanked, so a program written as one is
            # named by the literal that starts where the expression does.
            literal = next(
                (
                    value
                    for at, value in sorted(values.items())
                    if program.end() <= at <= end and not text[program.end() : at].strip()
                ),
                None,
            )
            if literal is not None:
                expression, written = literal, literal
            else:
                written = " ".join(expression.split())
            if not names_cargo(expression, tests):
                continue
            item = enclosing_item(tests, program.start())
            if STARTS.search(item):
                found.append((str(relative), rust_source.line_number(tests, program.start()), written))
    return found


def main() -> int:
    found = audit()
    for relative, line, program in found:
        print(
            f"{relative}:{line}: a test runs Cargo (`{program}`); the run's harness builds a "
            "binary a test starts, before the tests: `tilo_test_env::sibling_binary` for another "
            "package's (tools/build-sibling-binaries.sh, a nextest setup in .config/nextest.toml), "
            "`tilo_test_env::package_binary!` for the package's own",
            file=sys.stderr,
        )
    if found:
        print(f"{len(found)} test Cargo run(s) build the workspace a second time", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
