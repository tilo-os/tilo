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
ALLOW = re.compile(r"\ballow\s*\(")
REASON = re.compile(r"\breason\s*=")


class Attribute(NamedTuple):
    start_line: int
    end_line: int
    text: str


def rust_attributes(source: str) -> list[Attribute]:
    """Return balanced outer attributes, including multiline attributes."""
    attributes: list[Attribute] = []
    offset = 0
    while (start := source.find("#[", offset)) >= 0:
        depth = 1
        cursor = start + 2
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
    candidate = base if base and base != "null" else before
    if not candidate or candidate == ZERO_SHA:
        candidate = git(root, "rev-parse", "HEAD^")
    try:
        subprocess.run(
            ["git", "cat-file", "-e", f"{candidate}^{{commit}}"],
            cwd=root,
            check=True,
            capture_output=True,
        )
    except subprocess.CalledProcessError:
        subprocess.run(
            ["git", "fetch", "--no-tags", "--depth=1", "origin", candidate],
            cwd=root,
            check=True,
        )
    try:
        merge_base = git(root, "merge-base", candidate, "HEAD")
    except subprocess.CalledProcessError:
        return f"{candidate}..HEAD"
    return f"{merge_base}...HEAD"


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
        ],
        cwd=root,
    )
    return [
        Path(value.decode("utf-8", errors="surrogateescape"))
        for value in output.split(b"\0")
        if value
    ]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("base", nargs="?", default="")
    parser.add_argument("before", nargs="?", default="")
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    args = parser.parse_args(argv)
    root = args.root.resolve()
    try:
        revision_range = comparison_range(root, args.base, args.before)
        violations: list[tuple[Path, Attribute]] = []
        for relative in changed_rust_files(root, revision_range):
            source = (root / relative).read_text(encoding="utf-8")
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
