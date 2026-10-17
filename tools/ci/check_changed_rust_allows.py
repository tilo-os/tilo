#!/usr/bin/env python3
"""Require written reasons for Rust `allow` attributes in every changed file."""

from __future__ import annotations

import argparse
from bisect import bisect_right
import os
import re
import subprocess
import sys
from pathlib import Path
from typing import NamedTuple


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from policy.rust_source import lex, matching


ZERO_SHA = "0" * 40
ATTRIBUTE_START = re.compile(r"#!?\[")
ALLOW = re.compile(r"\ballow\s*\(")
REASON = re.compile(r"\breason\s*=")


class Attribute(NamedTuple):
    start_line: int
    end_line: int
    text: str


def _attributes(source: str, code: str):
    """Yield attributes and offsets using one lexical and newline index per file."""
    newlines = [match.start() for match in re.finditer("\n", source)]
    offset = 0
    while match := ATTRIBUTE_START.search(code, offset):
        start = match.start()
        end = matching(code, match.end() - 1)
        if code[end - 1 : end] != "]":
            break
        yield start, end, Attribute(
            start_line=bisect_right(newlines, start) + 1,
            end_line=bisect_right(newlines, end - 1) + 1,
            text=source[start:end],
        )
        offset = end


def rust_attributes(source: str) -> list[Attribute]:
    """Return real inner and outer attributes, excluding comments and literals."""
    code, _ = lex(source)
    return [attribute for _, _, attribute in _attributes(source, code)]


def unexplained_allows(source: str) -> list[Attribute]:
    # Do not lex the file or count its prefix for each attribute. Generated
    # bindings contain thousands of attributes in a single multi-megabyte file.
    code, literals = lex(source)
    literal_starts = [literal.start for literal in literals]
    violations: list[Attribute] = []
    for start, end, attribute in _attributes(source, code):
        for allow in ALLOW.finditer(code, start, end):
            close = matching(code, allow.end() - 1)
            valid_reason = False
            for reason in REASON.finditer(code, allow.end(), close):
                index = bisect_right(literal_starts, reason.end() - 1)
                if index == len(literals):
                    continue
                literal = literals[index]
                if (literal.end <= close and not literal.byte
                        and literal.value.strip()
                        and not code[reason.end():literal.start].strip()
                        and re.fullmatch(r"\s*,?\s*", code[literal.end:close - 1])):
                    valid_reason = True
                    break
            if not valid_reason:
                violations.append(attribute)
                break
    return violations


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
            "--diff-filter=ACMR",
            revision_range,
            "--",
            "*.rs",
            # Preserve imported upstream lint attributes without rewriting dependencies.
            ":(top,exclude)vendor/**",
        ],
        cwd=root,
    )
    # The comparison ends at HEAD, but the audit reads the working tree. A file
    # changed since the base may have been deleted in the index or worktree.
    # Trust Git's deletion status, not existence checks: unexpected read failures
    # (including files disappearing after this snapshot) must still fail closed.
    status = subprocess.check_output(
        ["git", "status", "--porcelain=v1", "-z", "--untracked-files=no"],
        cwd=root,
    )
    deleted: set[bytes] = set()
    records = iter(status.split(b"\0"))
    for record in records:
        if not record:
            continue
        flags, path = record[:2], record[3:]
        if b"D" in flags:
            deleted.add(path)
        if b"R" in flags or b"C" in flags:
            original = next(records)  # Original follows the destination with -z.
            if b"R" in flags:
                deleted.add(original)
    return [
        Path(value.decode("utf-8", errors="surrogateescape"))
        for value in output.split(b"\0")
        if value and value not in deleted
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


def staged_sources(root: Path, paths: list[Path]):
    """Read selected index blobs together, with NUL-delimited input paths.

    Git's length-delimited output preserves arbitrary blob contents. Checking
    every response fails closed on unresolved entries or missing/non-blob data.
    """
    if not paths:
        return
    output = subprocess.run(
        ["git", "cat-file", "--batch", "-z"], cwd=root,
        input=b"".join(b":" + os.fsencode(path.as_posix()) + b"\0" for path in paths),
        stdout=subprocess.PIPE, check=True,
    ).stdout
    offset = 0
    for path in paths:
        end = output.index(b"\n", offset)
        header = output[offset:end].split()
        if len(header) != 3 or header[1] != b"blob":
            raise ValueError(f"{path}: expected an indexed blob")
        length = int(header[2])
        start = end + 1
        offset = start + length + 1
        if length < 0 or output[offset - 1:offset] != b"\n":
            raise ValueError(f"{path}: incomplete indexed blob")
        yield path, output[start:offset - 1].decode("utf-8")
    if offset != len(output):
        raise ValueError("unexpected trailing indexed blob data")


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
        sources = (
            staged_sources(root, paths)
            if args.staged
            else ((relative, (root / relative).read_text(encoding="utf-8"))
                  for relative in paths)
        )
        for relative, source in sources:
            violations.extend(
                (relative, attribute)
                for attribute in unexplained_allows(source)
            )
    except (OSError, UnicodeError, ValueError, subprocess.CalledProcessError) as error:
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
