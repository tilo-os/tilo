#!/usr/bin/env python3
"""Keep date, time, number and size formatting in ``ui::format``.

Every date, time, size and duration a person reads goes through the toolkit's
formatter (``crates/sdk/ui-core/src/format.rs``), which follows their Region &
Formats settings and the desktop's language. Formatting built by hand drifts:
one app shows ``Sep 18, 2026, 12:30PM``, another ``Jun 30, 2026, 13:42``. This
check flags, in the production sources of the apps, the shell and session
programs and the SDK:

* ``strftime``-style patterns (``.format("%b %d")``) other than the ISO 8601
  machine forms (``%Y-%m-%d``, ``%Y-%m-%dT%H:%M:%S%.3fZ``) that files and
  protocols use;
* clock times built from parts (``format!("{h:02}:{m:02}")``);
* byte sizes with hand-written units (``format!("{n} MB")``, a ``["KB", "MB"]``
  table);
* month and weekday name tables (``["Jan", "Feb", …]``).

A line that formats for a machine (a protocol header, a log, a file name, a
timecode a format defines) says so with a ``// format: machine`` comment and
the reason, on that line or just above it, and is left alone. Test code is
exempt.
"""

from __future__ import annotations

import re
import sys
from dataclasses import dataclass
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from rust_source import is_test_file, line_number, production_source  # noqa: E402


ROOT = Path(__file__).resolve().parents[2]
SCOPES = (Path("apps"), Path("crates/session"), Path("crates/sdk"))
# The formatter itself, and the clock it formats times of day with.
FORMATTER = (
    Path("crates/sdk/ui-core/src/format.rs"),
    Path("crates/sdk/ui-core/src/format"),
    Path("crates/sdk/ui-core/src/clock.rs"),
)
MACHINE_MARK = re.compile(r"//\s*format:\s*machine\b")
# How many lines above a flagged line its mark may stand: a comment of a few
# lines over a table or a call.
MARK_REACH = 4

CHRONO_PATTERN = re.compile(r'\.format\(\s*"((?:[^"\\]|\\.)*%(?:[^"\\]|\\.)*)"')
MACHINE_PATTERN = re.compile(
    r"^(?:"
    r"%Y-?%m-?%d(?:[T_ ]%H[:-]?%M[:-]?%S(?:%\.\d?f|%f)?)?(?:Z|%z|%:z)?"
    r"|%Y-%m(?:-01)?"
    r"|%Y"
    r"|%Y%m%dT%H%M%SZ"
    r")$"
)
CLOCK_PARTS = re.compile(r"\{[A-Za-z_0-9.]*:02\}:\{[A-Za-z_0-9.]*:02\}")
STRING = re.compile(r'"((?:[^"\\\n]|\\.)*)"')
UNIT = r"(?:[KMGTPE]i?B|kB)(?:/s)?"
SIZED_STRING = re.compile(rf"\}}\s?{UNIT}\b")
UNIT_TABLE = re.compile(rf'"\s?{UNIT}"\s*,\s*"\s?{UNIT}"')
NAME_TABLE = re.compile(
    r'"(?:Jan|January)"\s*,\s*"(?:Feb|February)"'
    r'|"(?:Mon|Monday)"\s*,\s*"(?:Tue|Tuesday)"'
    r'|"(?:Sun|Sunday)"\s*,\s*"(?:Mon|Monday)"'
)


@dataclass(frozen=True)
class Violation:
    path: str
    line: int
    reason: str

    def __str__(self) -> str:
        return f"{self.path}:{self.line}: {self.reason}"


def in_formatter(relative: Path) -> bool:
    return any(relative == owned or owned in relative.parents for owned in FORMATTER)


def audit_text(relative: str, text: str) -> list[Violation]:
    """The hand-built formatting in one source file's production code."""
    raw_lines = text.splitlines()
    code = production_source(text)
    violations: list[Violation] = []

    def flag(offset: int, reason: str) -> None:
        line = line_number(code, offset)
        marked = raw_lines[max(line - 1 - MARK_REACH, 0) : line]
        if any(MACHINE_MARK.search(source) for source in marked):
            return
        violations.append(Violation(relative, line, reason))

    for found in CHRONO_PATTERN.finditer(code):
        if not MACHINE_PATTERN.match(found.group(1)):
            flag(
                found.start(),
                f'formats a date with "{found.group(1)}"; use a ui::format style',
            )
    for found in CLOCK_PARTS.finditer(code):
        flag(found.start(), "builds a clock time from parts; use ui::format::time")
    for found in STRING.finditer(code):
        if SIZED_STRING.search(found.group(1)):
            flag(found.start(), "writes a byte size by hand; use ui::format::size or memory")
    for found in UNIT_TABLE.finditer(code):
        flag(found.start(), "keeps its own byte unit table; use ui::format::size or memory")
    for found in NAME_TABLE.finditer(code):
        flag(found.start(), "keeps its own month or weekday names; use ui::format")
    # One report per line and reason.
    unique = {(v.line, v.reason): v for v in violations}
    return [unique[key] for key in sorted(unique)]


def audit(root: Path = ROOT) -> list[Violation]:
    violations: list[Violation] = []
    for scope in SCOPES:
        base = root / scope
        if not base.is_dir():
            continue
        for path in sorted(base.rglob("*.rs")):
            relative = path.relative_to(root)
            parts = relative.parts
            if "target" in parts or "src" not in parts:
                continue
            within_src = Path(*parts[parts.index("src") + 1 :])
            if is_test_file(within_src) or in_formatter(relative):
                continue
            text = path.read_text(encoding="utf-8", errors="replace")
            violations += audit_text(relative.as_posix(), text)
    return violations


def main() -> int:
    violations = audit()
    if violations:
        print("Formats contract FAILED: format through ui::format", file=sys.stderr)
        for violation in violations:
            print(f"  {violation}", file=sys.stderr)
        return 1
    print("Formats contract OK (dates, times, numbers and sizes go through ui::format)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
