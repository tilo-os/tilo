#!/usr/bin/env python3
"""Settings are written through typed keys, not string-literal sections.

``settings_store::update_section`` merges untyped JSON into a named section of
the settings document. A section name typed out at the call site fails
silently when it is misspelt, and a value of the wrong type is stored without
complaint: the reader falls back to its default and the setting seems to do
nothing. Production code names a setting through ``settings_store::Key`` (one
leaf) or ``settings_store::SectionWrite`` (several leaves of one section), or,
for a writer that owns a whole section, through the schema's own constant.

This check refuses a string literal as the section argument of the raw writes
(``update_section``, ``update_section_by``, ``update_section_report``,
``compare_update_section`` and their ``_at`` forms, whose section is the
second argument) in production sources outside ``settings-store`` itself.
Test modules and test files are exempt: a test seeds the document it reads.
"""

from __future__ import annotations

import re
import sys
from dataclasses import dataclass
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from rust_source import is_test_file, line_number, production_source, skip_string  # noqa: E402


ROOT = Path(__file__).resolve().parents[2]
SCANNED = (Path("crates"), Path("apps"))
# The crate that defines the raw write, its keys and its tests.
ALLOWED = (Path("crates/system/settings-store"),)

# Writes whose first argument is the section.
SECTION_FIRST = ("update_section", "update_section_by", "update_section_report", "compare_update_section")
# Writes whose first argument is a path and second the section.
SECTION_SECOND = ("update_section_at", "compare_update_section_at")
CALL = re.compile(
    r"\b(?P<name>" + "|".join(sorted(SECTION_FIRST + SECTION_SECOND, key=len, reverse=True)) + r")\s*\("
)
STRING_LITERAL = re.compile(r'^(?:b?"|b?r#*")')


@dataclass(frozen=True)
class Violation:
    path: str
    line: int
    call: str

    def __str__(self) -> str:
        return (
            f"{self.path}:{self.line}: {self.call}(...) names its section with a string "
            "literal; write through a settings_store::Key or SectionWrite, or the "
            "schema's section constant"
        )


def arguments(text: str, open_paren: int) -> list[str]:
    """The top-level arguments of the call whose ``(`` is at ``open_paren``."""
    args: list[str] = []
    depth = 0
    start = open_paren + 1
    index = start
    while index < len(text):
        char = text[index]
        previous = text[index - 1]
        starts_prefixed = (
            char in "br"
            and not (previous.isalnum() or previous == "_")
            and re.match(r'(?:b?r#*|b)"', text[index:])
        )
        if char == '"' or starts_prefixed:
            index = skip_string(text, index)
            continue
        if char in "([{":
            depth += 1
        elif char in ")]}":
            if depth == 0:
                args.append(text[start:index])
                return [arg.strip() for arg in args]
            depth -= 1
        elif char == "," and depth == 0:
            args.append(text[start:index])
            start = index + 1
        index += 1
    return [arg.strip() for arg in args]


def audit_file(root: Path, path: Path) -> list[Violation]:
    relative = path.relative_to(root)
    text = path.read_text(encoding="utf-8")
    if not CALL.search(text):
        return []
    text = production_source(text)
    violations = []
    for call in CALL.finditer(text):
        name = call.group("name")
        # A definition (`fn update_section(`) is not a call.
        if re.search(r"\bfn\s+$", text[max(0, call.start() - 8) : call.start()]):
            continue
        args = arguments(text, call.end() - 1)
        position = 1 if name in SECTION_SECOND else 0
        if len(args) > position and STRING_LITERAL.match(args[position]):
            violations.append(Violation(relative.as_posix(), line_number(text, call.start()), name))
    return violations


def audit(root: Path = ROOT) -> list[Violation]:
    violations: list[Violation] = []
    for top in SCANNED:
        base = root / top
        if not base.is_dir():
            continue
        for path in sorted(base.rglob("*.rs")):
            relative = path.relative_to(root)
            if any(relative.is_relative_to(allowed) for allowed in ALLOWED):
                continue
            if "target" in relative.parts or is_test_file(relative):
                continue
            violations += audit_file(root, path)
    return violations


def main() -> int:
    violations = audit()
    if violations:
        print("Settings write contract FAILED", file=sys.stderr)
        for violation in violations:
            print(f"  {violation}", file=sys.stderr)
        return 1
    print("Settings write contract OK (no string-literal settings sections outside settings-store)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
