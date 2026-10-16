#!/usr/bin/env python3
"""Require written reasons for Rust `allow` attributes in every changed file."""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path
from typing import NamedTuple


ZERO_SHA = "0" * 40
ATTRIBUTE_START = re.compile(r"#!?\[")
ALLOW = re.compile(r"\ballow\s*\(")
REASON = re.compile(r"\breason\s*=")


class Attribute(NamedTuple):
    start_line: int
    end_line: int
    text: str


def rust_attributes(source: str) -> list[Attribute]:
    """Return balanced inner and outer attributes, including multiline attributes."""
    attributes: list[Attribute] = []
    offset = 0
    while match := ATTRIBUTE_START.search(source, offset):
        start = match.start()
        depth = 1
        cursor = match.end()
        quote: str | None = None
        escaped = False
        while cursor < len(source) and depth:
            char = source[cursor]
            if quote is not None:
                if escaped:
                    escaped = False
                elif char == "\\":
                    escaped = True
                elif char == quote:
                    quote = None
            elif char in {'"', "'"}:
                quote = char
            elif char == "[":
                depth += 1
            elif char == "]":
                depth -= 1
            cursor += 1
        if depth:
            break
        text = source[start:cursor]
        attributes.append(
            Attribute(
                start_line=source.count("\n", 0, start) + 1,
                end_line=source.count("\n", 0, cursor) + 1,
                text=text,
            )
        )
        offset = cursor
    return attributes


def unexplained_allows(source: str) -> list[Attribute]:
    return [
        attribute
        for attribute in rust_attributes(source)
        if ALLOW.search(attribute.text)
        and not REASON.search(attribute.text)
    ]


def git(root: Path, *arguments: str) -> str:
    return subprocess.check_output(
        ["git", *arguments], cwd=root, text=True, errors="surrogateescape"
    ).strip()


def comparison_range(root: Path, base: str, before: str) -> str:
    """PRs compare their common ancestor; pushes compare the exact previous tree.

    A shallow checkout cannot safely substitute the current target branch tree
    for a missing PR merge base: that would attribute target-only edits to the PR.
    Fetch both immutable endpoints, deepening only when ancestry is missing.
    """
    is_pr = bool(base and base != "null")
    candidate = base if is_pr else before
    if not candidate or candidate in {"null", ZERO_SHA}:
        candidate = git(root, "rev-parse", "HEAD^")
    try:
        subprocess.run(
            ["git", "cat-file", "-e", f"{candidate}^{{commit}}"],
            cwd=root, check=True, capture_output=True,
        )
    except subprocess.CalledProcessError:
        subprocess.run(
            ["git", "fetch", "--no-tags", "--depth=1", "origin", candidate],
            cwd=root, check=True,
        )
    if not is_pr:
        return f"{candidate}..HEAD"

    head = git(root, "rev-parse", "HEAD")
    for deepen in ("--deepen=64", "--deepen=256", "--deepen=1024", "--unshallow", None):
        try:
            merge_base = git(root, "merge-base", candidate, head)
            return f"{merge_base}..HEAD"
        except subprocess.CalledProcessError:
            if deepen is None or git(root, "rev-parse", "--is-shallow-repository") != "true":
                raise
        subprocess.run(
            ["git", "fetch", "--no-tags", deepen, "origin", candidate, head],
            cwd=root, check=True,
        )
    raise AssertionError("unreachable comparison range")


def changed_rust_files(root: Path, revision_range: str) -> list[Path]:
    output = subprocess.check_output(
        [
            "git",
            "diff",
            "--name-only",
            "-z",
            "--diff-filter=ACMRTUXB",
            revision_range,
            "--",
            "*.rs",
            # Preserve imported upstream lint attributes without rewriting dependencies.
            ":(top,exclude)vendor/**",
        ],
        cwd=root,
    )
    return [
        Path(value.decode("utf-8", errors="surrogateescape"))
        for value in output.split(b"\0")
        if value
    ]


def staged_rust_files(root: Path) -> list[Path]:
    output = subprocess.check_output(
        [
            "git",
            "diff",
            "--cached",
            "--name-only",
            "-z",
            "--diff-filter=ACMRTUXB",
            "--",
            "*.rs",
            ":(top,exclude)vendor/**",
        ],
        cwd=root,
    )
    return [
        Path(value.decode("utf-8", errors="surrogateescape"))
        for value in output.split(b"\0")
        if value
    ]


def staged_source(root: Path, relative: Path) -> str:
    """Read the exact staged blob, not a possibly corrected working-tree file."""
    return subprocess.check_output(
        ["git", "show", f":{relative.as_posix()}"], cwd=root
    ).decode("utf-8")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("base", nargs="?", default="")
    parser.add_argument("before", nargs="?", default="")
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--comparison-range", action="store_true",
                        help="print the shared PR/push range for the formatter")
    parser.add_argument(
        "--staged",
        action="store_true",
        help="check the exact Rust blobs in the index (for pre-commit)",
    )
    args = parser.parse_args(argv)
    root = args.root.resolve()
    try:
        if args.staged and (args.base or args.before or args.comparison_range):
            parser.error("--staged cannot be combined with revisions or --comparison-range")
        revision_range = (
            "staged index"
            if args.staged
            else comparison_range(root, args.base, args.before)
        )
        if args.comparison_range:
            print(revision_range)
            return 0
        violations: list[tuple[Path, Attribute]] = []
        paths = (
            staged_rust_files(root)
            if args.staged
            else changed_rust_files(root, revision_range)
        )
        for relative in paths:
            source = (
                staged_source(root, relative)
                if args.staged
                else (root / relative).read_text(encoding="utf-8")
            )
            violations.extend(
                (relative, attribute)
                for attribute in unexplained_allows(source)
            )
    except (OSError, UnicodeError, subprocess.CalledProcessError) as error:
        print(f"Rust allow-reason check failed: {error}", file=sys.stderr)
        return 1
    if violations:
        for path, attribute in violations:
            lints = " ".join(attribute.text.split())
            print(
                f"{path}:{attribute.start_line}: changed file contains allow with no reason: {lints}",
                file=sys.stderr,
            )
        print(
            "FAIL: add `reason = \"...\"` to every Rust allow in changed files",
            file=sys.stderr,
        )
        return 1
    print(f"changed Rust allow reasons OK ({revision_range})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
