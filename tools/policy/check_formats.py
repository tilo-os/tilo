#!/usr/bin/env python3
"""Keep date, time, number and size formatting in ``ui::format``.

Every date, time, size and duration a person reads goes through the toolkit's
formatter (``crates/sdk/ui-core/src/format.rs``, and the toolkit-free
``crates/sdk/tilo-formats`` it re-exports), which follows their Region & Formats
settings and the desktop's language. Formatting built by hand drifts:
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

A value that formats for a machine (a protocol field, a log, a file name, a
timecode a format defines) says so in code, in the call that keeps it::

    let name = ui::format::machine!("the record's file name", format!("{:04}.json", id));

`tilo_formats::machine!` (the toolkit re-exports it as `ui::format::machine!`)
is the expression it wraps, and its first argument is the reason. The reason is
code the compiler keeps, so tidying the comments around it never takes it away;
this check reads the call, never a comment. Test code is exempt.
"""

from __future__ import annotations

import argparse
import re
import sys
from dataclasses import dataclass
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from judged import add_staged_argument, judged  # noqa: E402
from rust_source import is_test_file, line_number, matching, production_source  # noqa: E402


ROOT = Path(__file__).resolve().parents[2]
SCOPES = (Path("apps"), Path("crates/session"), Path("crates/sdk"))
# The formatter itself, its toolkit-free half, and the clock it formats times of day with.
FORMATTER = (
    Path("crates/sdk/tilo-formats/src"),
    Path("crates/sdk/ui-core/src/format.rs"),
    Path("crates/sdk/ui-core/src/format"),
    Path("crates/sdk/ui-core/src/clock.rs"),
)
MACHINE = re.compile(r"\bmachine\s*!\s*\(")
FIRST_ARGUMENT = re.compile(r'\s*"((?:[^"\\]|\\.)*)"\s*,')
# The reason has to say why the value is machine text: a call with a word or two
# is not an account a reader can check, so it leaves the finding where it was.
REASON_WORDS = 6

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


def machine_spans(code: str) -> list[tuple[int, int]]:
    """The spans `machine!("<why>", <expr>)` covers, each from the call's name to
    the bracket that closes it.

    The reason is the call's first argument: text inside the call is text the
    reason accounts for. A call whose reason says too little covers nothing.
    """
    spans: list[tuple[int, int]] = []
    for found in MACHINE.finditer(code):
        open_paren = found.end() - 1
        close = matching(code, open_paren)
        reason = FIRST_ARGUMENT.match(code, open_paren + 1)
        if reason is None or len(reason.group(1).split()) < REASON_WORDS:
            continue
        spans.append((found.start(), close))
    return spans


def audit_text(relative: str, text: str) -> list[Violation]:
    """The hand-built formatting in one source file's production code."""
    code = production_source(text)
    for_machines = machine_spans(code)
    violations: list[Violation] = []

    def flag(offset: int, reason: str) -> None:
        if any(start <= offset < end for start, end in for_machines):
            return
        violations.append(Violation(relative, line_number(code, offset), reason))

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


def audit(root: Path = ROOT, staged: bool = False) -> list[Violation]:
    violations: list[Violation] = []
    files = judged(root, staged)
    for scope in SCOPES:
        for relative, text in files.sources(scope):
            parts = relative.parts
            if "target" in parts or "src" not in parts:
                continue
            within_src = Path(*parts[parts.index("src") + 1 :])
            if is_test_file(within_src) or in_formatter(relative):
                continue
            violations += audit_text(relative.as_posix(), text)
    return violations


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    add_staged_argument(parser)
    violations = audit(staged=parser.parse_args(argv).staged)
    if violations:
        print("Formats contract FAILED: format through ui::format", file=sys.stderr)
        for violation in violations:
            print(violation)
        return 1
    print("Formats contract OK (dates, times, numbers and sizes go through ui::format)", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
