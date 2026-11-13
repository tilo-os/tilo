#!/usr/bin/env python3
"""A versioned on-disk format never stops reading what a machine may hold.

User data survives every update (docs/architecture/compatibility.md). A
program that writes a file in a format of its own declares it as a
``tilo_files::native::migration::Format``: its version, the oldest version it
still brings up, and one step per version in between. A machine that missed
updates still holds a file of an older version, so the chain only grows:

``check`` (the repository policy suite) fails when a declaration's
``oldest`` is not a plain number, which the comparison below needs, and when
no loader brings the format's stored versions up: a declared chain of steps
only keeps data if the code that opens the file runs it. Each format's crate
must call ``<FORMAT>.bring_up(path, bytes, stored)`` (or ``bring_up_moved``
for a version that lived in a file of its own) with the version it read from
the file, not a number it assumed, so every stored version goes through the
steps and leaves its old file beside the new one; and no loader calls
``migrate``, which brings bytes up but keeps no copy of what the file held.
A loader that brought up only the first version's file and read a later one
as it was refused a whole relay's state at start.

``against BASE`` (run against main before landing) fails when a format
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
    r"(?<!\w)Format\s*\{\s*name:\s*\"(?P<name>[^\"]+)\"\s*,\s*version:\s*[^,]+,\s*"
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


BINDING = re.compile(
    r"\bconst\s+(?P<binding>[A-Z][A-Z0-9_]*)\s*:\s*(?:[\w:]+::)?Format\s*=\s*"
    r"(?:[\w:]+::)?Format\s*\{\s*name:\s*\"(?P<name>[^\"]+)\"",
    re.S,
)
CALL = r"\s*\.\s*(?P<call>bring_up_moved|bring_up|migrate)\s*\("


def without_tests(path: str, text: str) -> str:
    """`text` without its tests: nothing for a test file, else everything
    before its first inline `#[cfg(test)] mod … {`, where this tree keeps a
    file's tests."""
    name = path.rsplit("/", 1)[-1]
    if "/tests/" in path or "/benches/" in path or "/examples/" in path:
        return ""
    if name.endswith("tests.rs") or name in ("test_support.rs", "fixtures.rs"):
        return ""
    inline = re.search(r"#\[cfg\(test\)\]\s*(?:pub(?:\([\w:]+\))?\s+)?mod\s+\w+\s*\{", text)
    return text[: inline.start()] if inline else text


def arguments(text: str, start: int) -> list[str]:
    """The top-level, comma-separated arguments of the call whose opening
    parenthesis is just before `start`."""
    depth, current, found = 0, [], []
    for char in text[start:]:
        if char in "([{":
            depth += 1
        elif char in ")]}":
            if depth == 0:
                found.append("".join(current).strip())
                return [argument for argument in found if argument]
            depth -= 1
        elif char == "," and depth == 0:
            found.append("".join(current).strip())
            current = []
            continue
        current.append(char)
    return found


def crate_of(root: Path, path: str) -> Path:
    """The directory of the crate `path` (relative to `root`) belongs to, or
    the file's own directory when no manifest is above it."""
    directory = (root / path).parent
    while directory != root and not (directory / "Cargo.toml").is_file():
        directory = directory.parent
    return directory if directory != root else (root / path).parent


def loaders(root: Path, texts: dict[str, str]) -> list[str]:
    """Each declared format whose crate opens its file without bringing every
    stored version up through `bring_up`, or brings one up with `migrate`."""
    problems = []
    crates: dict[Path, dict[str, str]] = {}
    for path, text in texts.items():
        for match in BINDING.finditer(text):
            binding, name = match.group("binding"), match.group("name")
            crate = crate_of(root, path)
            if crate not in crates:
                crates[crate] = {
                    file.relative_to(root).as_posix(): file.read_text(errors="replace")
                    for file in crate.glob("**/*.rs")
                }
            stem = path.rsplit("/", 1)[-1].removesuffix(".rs")
            named = []
            for other, other_text in crates[crate].items():
                code = without_tests(other, other_text)
                bare = other == path or re.search(rf"\buse\b[^;]*\b{stem}::{binding}\b", code)
                prefix = rf"(?<![\w:]){binding}" if bare else rf"\b{stem}::{binding}"
                for call in re.finditer(prefix + CALL, code):
                    named.append((other, call.group("call"), arguments(code, call.end())))
            for other, call, args in named:
                if call == "migrate":
                    problems.append(
                        f"{other}: format {name!r} is brought up with `migrate`, which keeps no "
                        "copy of what the file held; a loader calls `bring_up` (or "
                        "`bring_up_moved` for a version kept in a file of its own)"
                    )
            if not any(call != "migrate" and args and not args[-1].isdigit() for _, call, args in named):
                problems.append(
                    f"{path}: format {name!r} declares the versions it brings up, but no code "
                    f"of its crate passes the version it read from the file to "
                    f"`{binding}.bring_up`: a file an older build wrote would be read as it is, or "
                    "refused. Its loader reads the stored version and brings it up before it reads."
                )
    return problems


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
    texts = sources(root)
    for name, (path, oldest) in declarations(texts).items():
        if not oldest.isdigit():
            problems.append(
                f"{path}: format {name!r} declares `oldest: {oldest}`; write the number, so "
                "that it can only ever fall"
            )
    problems.extend(loaders(root, texts))
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
