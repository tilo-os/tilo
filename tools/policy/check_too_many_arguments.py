#!/usr/bin/env python3
"""No function takes more than eight parameters, and nothing opts out.

The workspace denies ``clippy::too_many_arguments`` and the root
``clippy.toml`` sets its threshold to eight. A function over the limit is
redesigned, never excused, so this check rejects every way of silencing the
lint:

* an ``allow``/``expect`` attribute (plain, inner or under ``cfg_attr``)
  naming ``clippy::too_many_arguments`` in a Rust source;
* a Cargo manifest setting the lint to anything but ``deny``/``forbid``.

Only the repository's top-level vendor/ third-party crates are exempt.
Source scope comes from Git: tracked (including newly staged) files and
non-ignored untracked files, never ignored image assembly or registry caches.
Both working-tree files and indexed blobs are checked.
"""

from __future__ import annotations

import os
import re
import subprocess
import sys
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
LINT = "too_many_arguments"
ATTRIBUTE = re.compile(r"#!?\[")
SILENCING = re.compile(r"\b(?:allow|expect)\s*\(")


def git(root: Path, *args: str, input: bytes | None = None) -> bytes:
    return subprocess.run(
        ["git", "-C", str(root), *args],
        input=input, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True,
    ).stdout


def own_source(relative: Path) -> bool:
    return relative.parts[0] != "vendor" and (
        relative.suffix == ".rs" or relative.name == "Cargo.toml"
    )


def repository_sources(root: Path):
    names = git(root, "ls-files", "--cached", "--others", "--exclude-standard", "-z")
    for name in sorted(set(names.split(b"\0")) - {b""}):
        relative = Path(os.fsdecode(name))
        path = root / relative
        if own_source(relative) and path.is_file():
            yield relative, path.read_text(encoding="utf-8", errors="replace")

    # Check the index too: an unstaged edit must not hide a staged opt-out.
    entries = []
    for record in git(root, "ls-files", "--stage", "-z").split(b"\0"):
        if not record:
            continue
        metadata, name = record.split(b"\t", 1)
        mode, oid, _stage = metadata.split()
        relative = Path(os.fsdecode(name))
        if own_source(relative) and mode in {b"100644", b"100755"}:
            entries.append((relative, oid))
    if not entries:
        return
    blobs = git(root, "cat-file", "--batch", input=b"".join(oid + b"\n" for _, oid in entries))
    offset = 0
    for relative, _ in entries:
        end = blobs.index(b"\n", offset)
        _, kind, size = blobs[offset:end].split()
        if kind != b"blob":
            raise ValueError(f"{relative}: expected an indexed blob")
        offset = end + 1
        length = int(size)
        yield relative, blobs[offset : offset + length].decode("utf-8", errors="replace")
        offset += length + 1


def attributes(text: str):
    """Yield ``(offset, body)`` for every ``#[...]``/``#![...]`` attribute."""
    for match in ATTRIBUTE.finditer(text):
        depth = 0
        for index in range(match.end() - 1, len(text)):
            if text[index] == "[":
                depth += 1
            elif text[index] == "]":
                depth -= 1
                if depth == 0:
                    yield match.start(), text[match.end() : index]
                    break


def audit_source(relative: Path, text: str) -> list[str]:
    return [
        f"{relative}:{text.count(chr(10), 0, offset) + 1}: silences clippy::{LINT}; "
        "redesign the function to take at most eight parameters"
        for offset, body in attributes(text)
        if SILENCING.search(body) and re.search(rf"\bclippy::{LINT}\b", body)
    ]


def audit_manifest(relative: Path, text: str) -> list[str]:
    try:
        manifest = tomllib.loads(text)
    except tomllib.TOMLDecodeError as error:
        return [f"{relative}: cannot audit malformed Cargo manifest: {error}"]
    lints = manifest.get("lints", {})
    tables = [lints, manifest.get("workspace", {}).get("lints", {})]
    violations = []
    for table in tables:
        level = table.get("clippy", {}).get(LINT) if isinstance(table, dict) else None
        if isinstance(level, dict):
            level = level.get("level")
        if level is not None and level not in {"deny", "forbid"}:
            violations.append(f"{relative}: sets clippy::{LINT} to {level!r}; it must stay denied")
    return violations


def audit(root: Path) -> list[str]:
    violations: list[str] = []
    for relative, text in repository_sources(root):
        if relative.suffix == ".rs":
            violations.extend(audit_source(relative, text))
        else:
            violations.extend(audit_manifest(relative, text))
    return sorted(set(violations))


def main() -> int:
    violations = audit(ROOT)
    for violation in violations:
        print(violation)
    if violations:
        return 1
    print(f"parameter limit OK (no clippy::{LINT} escape hatch)", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
