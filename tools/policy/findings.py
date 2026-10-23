#!/usr/bin/env python3
"""Run a policy step and report what it found one finding at a time, as t compares tests.

  findings.py COMMAND [ARG...]

The step's output is passed through as it is. When the step reports problems (exit 1, no
traceback), each line it printed is also reported as a failed test, `test <finding> ... FAILED`,
with the line under `---- <finding> stdout ----`: t compares a check's failed tests with the
same check's on main (docs/t.md), so a change passes a step that main fails only when every
finding it has, main has too, and a change that adds one is refused even while main is red.

A finding is its line with the numbers in it blanked (`apps/a.rs: N lines, over its ceiling of
N`), so a count or a line number that moves is the same finding; the second of the same line is
`<finding> (2)`, so one more of what main has is a finding main doesn't have. A step that
crashed or was stopped found nothing: it fails as a whole, compared with main as a whole.
"""
from __future__ import annotations

import re
import subprocess
import sys

# A run of digits that is a number of its own (a count, a line number, `3019/3000`), not part
# of a name (`v2`, `x86_64`, `sha256`).
NUMBER = re.compile(r"(?<![A-Za-z_\d])\d+(?![A-Za-z_\d])")
# A temporary directory's random name.
TEMPORARY = re.compile(r"/tmp/[^\s/:'\"]+")
# What t reads as a test's result line or as the header of its output: never part of a
# finding's name.
RESERVED = (" ... ", "---- ", " stdout ----")
LONGEST = 240


def finding(line: str) -> str:
    """The finding a line of a step's output reports, or "" for a blank one."""
    text = " ".join(line.split())
    text = TEMPORARY.sub("/tmp/…", NUMBER.sub("N", text))
    for reserved in RESERVED:
        text = text.replace(reserved, " ")
    text = text.strip()
    return text if len(text) <= LONGEST else text[:LONGEST] + "…"


def findings(output: str) -> list[tuple[str, str]]:
    """Each finding of `output` with the line it came from, the repeats of one numbered."""
    seen: dict[str, int] = {}
    found = []
    for line in output.splitlines():
        name = finding(line)
        if not name:
            continue
        seen[name] = seen.get(name, 0) + 1
        found.append((name if seen[name] == 1 else f"{name} ({seen[name]})", line.rstrip()))
    return found


def found_nothing(status: int, output: str) -> bool:
    """Whether the step failed without reporting problems: it crashed, was stopped, or was used
    wrongly (exit 2). Only exit 1 with no traceback is the step saying what it found."""
    return status != 1 or "Traceback (most recent call last)" in output


def report(found: list[tuple[str, str]]) -> str:
    """As libtest reports: each test's line, then each one's output, which ends at the next
    header or at the result line."""
    lines = ["", *(f"test {name} ... FAILED" for name, _ in found), "", "failures:", ""]
    for name, line in found:
        lines += [f"---- {name} stdout ----", line, ""]
    lines += [f"test result: FAILED. 0 passed; {len(found)} failed", ""]
    return "\n".join(lines)


def main(argv: list[str]) -> int:
    if not argv:
        print(__doc__, file=sys.stderr)
        return 2
    run = subprocess.run(argv, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL)
    output = run.stdout.decode("utf-8", errors="replace")
    sys.stdout.write(output)
    if run.returncode != 0 and not found_nothing(run.returncode, output):
        found = findings(output)
        if found:
            sys.stdout.write(report(found))
    sys.stdout.flush()
    return run.returncode if run.returncode >= 0 else 128 - run.returncode


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
