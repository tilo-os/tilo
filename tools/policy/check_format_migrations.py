#!/usr/bin/env python3
"""A versioned on-disk format never stops reading what a machine may hold.

User data survives every update (docs/architecture/compatibility.md). A
program that writes a file in a format of its own declares it as a
``tilo_files::native::migration::Format``: its version, the oldest version it
still brings up, and one step per version in between. A machine that missed
updates still holds a file of an older version, so the chain only grows:

``check`` (the repository policy suite) fails when a declaration's
``oldest`` is not a plain number, which the comparison below needs.

``against BASE`` (t's ``changes`` check, against main) fails when a format
declared at BASE now brings up fewer versions: its ``oldest`` rose, or the
declaration is gone while its file may still be on disk.

Each step is tested against a file its previous version wrote, and only that
(``assert_step_reads_previous``, beside each declaration).
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DECLARATION = re.compile(
    r"migration::Format\s*\{\s*name:\s*\"(?P<name>[^\"]+)\"\s*,\s*version:\s*[^,]+,\s*"
    r"oldest:\s*(?P<oldest>[^,]+?)\s*,",
    re.S,
)


def declarations(texts: dict[str, str]) -> dict[str, tuple[str, str]]:
    """Format name -> (file, oldest as written)."""
    found = {}
    for path, text in texts.items():
        for match in DECLARATION.finditer(text):
            found[match.group("name")] = (path, match.group("oldest").strip())
    return found


def sources(root: Path) -> dict[str, str]:
    texts = {}
    for path in root.glob("crates/**/*.rs"):
        if "/target/" in path.as_posix():
            continue
        text = path.read_text(errors="replace")
        if "migration::Format" in text:
            texts[path.relative_to(root).as_posix()] = text
    for path in root.glob("apps/**/*.rs"):
        text = path.read_text(errors="replace")
        if "migration::Format" in text:
            texts[path.relative_to(root).as_posix()] = text
    return texts


def sources_at(root: Path, revision: str) -> dict[str, str]:
    listed = subprocess.run(
        ["git", "grep", "-l", "migration::Format", revision, "--", "*.rs"],
        cwd=root,
        capture_output=True,
        text=True,
    )
    texts = {}
    for line in listed.stdout.splitlines():
        _, _, path = line.partition(":")
        shown = subprocess.run(
            ["git", "show", f"{revision}:{path}"], cwd=root, capture_output=True, text=True
        )
        if shown.returncode == 0:
            texts[path] = shown.stdout
    return texts


def sources_since(root: Path, base: str, before: dict[str, str]) -> dict[str, str]:
    """What declares a format now: `before` (the sources at `base`) with the
    files changed since, and the new ones git doesn't track yet, read again.
    Only those: reading every file of the tree took most of the check on a
    node whose disk is busy."""
    def listed(*arguments: str) -> list[str]:
        out = subprocess.run(
            ["git", *arguments, "--", "*.rs"], cwd=root, capture_output=True, text=True, check=True
        ).stdout
        return [path for path in out.split("\0") if path]

    texts = dict(before)
    changed = listed("diff", "--name-only", "-z", "--no-renames", base)
    for path in changed + listed("ls-files", "-z", "--others", "--exclude-standard"):
        texts.pop(path, None)
        file = root / path
        if file.is_file():
            text = file.read_text(errors="replace")
            if "migration::Format" in text:
                texts[path] = text
    return texts


def check(root: Path = ROOT) -> list[str]:
    problems = []
    for name, (path, oldest) in declarations(sources(root)).items():
        if not oldest.isdigit():
            problems.append(
                f"{path}: format {name!r} declares `oldest: {oldest}`; write the number, so "
                "that it can only ever fall"
            )
    return problems


def against(base: str, root: Path = ROOT) -> list[str]:
    problems = []
    then = sources_at(root, base)
    before = declarations(then)
    now = declarations(sources_since(root, base, then))
    for name, (path, oldest) in before.items():
        if name not in now:
            problems.append(
                f"{path}: format {name!r} is no longer declared; files a machine holds in it "
                "would no longer load"
            )
            continue
        new_path, new_oldest = now[name]
        if oldest.isdigit() and new_oldest.isdigit() and int(new_oldest) > int(oldest):
            problems.append(
                f"{new_path}: format {name!r} no longer brings up version {oldest}; a machine "
                "that missed updates may still hold it. Keep its step."
            )
    return problems


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("check")
    base = sub.add_parser("against")
    base.add_argument("base")
    args = parser.parse_args(argv)
    if args.command == "check":
        problems = check()
        for problem in problems:
            print(problem)
    else:
        problems = against(args.base)
        for problem in problems:
            print(f"::error::{problem}", file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
