#!/usr/bin/env python3
"""No function takes more than eight parameters, and nothing opts out.

The workspace denies ``clippy::too_many_arguments`` and the root
``clippy.toml`` sets its threshold to eight. A function over the limit is
redesigned, never excused, so this check rejects every way of silencing the
lint:

* an ``allow``/``expect`` attribute (plain, inner or under ``cfg_attr``)
  naming ``clippy::too_many_arguments`` in a Rust source;
* a Cargo manifest setting the lint to anything but ``deny``/``forbid``.

Vendored third-party crates are exempt.
"""

from __future__ import annotations

import os
import re
import sys
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
LINT = "too_many_arguments"
SKIPPED_DIRS = {".git", ".claude", "target", "vendor", "node_modules"}
ATTRIBUTE = re.compile(r"#!?\[")
SILENCING = re.compile(r"\b(?:allow|expect)\s*\(")


def repository_files(root: Path, name_matches) -> list[Path]:
    found: list[Path] = []
    for directory, subdirs, files in os.walk(root):
        subdirs[:] = sorted(d for d in subdirs if d not in SKIPPED_DIRS)
        found.extend(Path(directory) / f for f in sorted(files) if name_matches(f))
    return found


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
    for path in repository_files(root, lambda name: name.endswith(".rs")):
        text = path.read_text(encoding="utf-8", errors="replace")
        if LINT in text:
            violations.extend(audit_source(path.relative_to(root), text))
    for path in repository_files(root, lambda name: name == "Cargo.toml"):
        text = path.read_text(encoding="utf-8")
        violations.extend(audit_manifest(path.relative_to(root), text))
    return violations


def main() -> int:
    violations = audit(ROOT)
    for violation in violations:
        print(violation, file=sys.stderr)
    if violations:
        return 1
    print(f"parameter limit OK (no clippy::{LINT} escape hatch)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
