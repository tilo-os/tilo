#!/usr/bin/env python3
"""Run a policy step and report what it found one finding at a time, as t compares tests.

  findings.py COMMAND [ARG...]

The step's output is passed through as it is. When the step reports problems (exit 1, no
traceback), each line it printed is also reported as a failed test, `test <finding> ... FAILED`,
with the line under `---- <finding> stdout ----`: t compares a check's failed tests with the
same check's on main (docs/t.md), so a change passes a step that main fails only when every
finding it has, main has too, and a change that adds one is refused even while main is red.

A finding is named by the file it is about, never by the words a step used to say it: a line
that names a file of the tree is about the last one it names, so `apps/b.rs: 3019 lines, over its
ceiling of 3000; split it` and `apps/b.rs: 3412 lines, over its ceiling of 3500; split its
cohesive` are the same finding when a ceiling moves, and a second finding about a file is
`apps/b.rs (2)`, so one more of what main has is a finding main doesn't have. A line naming no
file of the tree keeps the step's own words with the numbers in them blanked. A summary line
that says how many a step found (`source file size: 3 problem(s); …`) is not a finding of its
own: it counts the ones above it, and makes every step that grows a problem look new.

That is what keeps a step already red on main honest: a file added past its ceiling is the
branch's finding even while main is red for other files, and main's findings for those files
stay main's. A line about a file that is no longer there is a finding no step of the branch
reports, which costs a push that removes one nothing: t compares what fails, not what passes.

A step that crashed or was stopped found nothing: it fails as a whole, compared with main as a
whole.
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
# `<file> more: <what the second finding about it says>`, the name of a finding whose step
# reports two about one file: a file first, then what the line says after it.
REPEAT = re.compile(r"^(?P<file>[^\s\[]+) that finding: (?P<more>.*)$")
# A path of the tree a finding names: the file it is about. Paths with dots in them are files
# (`os/Containerfile.env-runtime`, `Cargo.toml`, `apps/a.rs`), and so are the extensionless
# names at the tree's top (`Containerfile`, `Makefile`, `Dockerfile`).
PATH = re.compile(r"(?<![\w./-])(?:(?:apps|crates|dist|docs|examples|experiments|fuzz|keys|os|scripts|tools)"
                  r"(?:/[\w.-]+)+|(?:[\w-]+/)*[\w-]+\.[A-Za-z0-9]+|(?:Makefile|Dockerfile|Containerfile))"
                  r"(?![\w./-])")
# A line that counts what it found instead of naming it: `<rule>: N problem(s)[; …]`, or the
# count alone (`N image builder input problem(s)`).
SUMMARY = re.compile(r"^(?:[A-Za-z][\w .'-]*?: )?\d+ (?:[a-z]+ )*(?:problem|finding|error|violation|warning)s?\b")


def spaced(line: str) -> str:
    """A line with its runs of spaces squeezed, so that a step that indents a finding and one
    that does not are read alike."""
    return " ".join(line.split())


def more(said: str, about: str) -> str:
    """The name of a finding when its step reports two or more about one file: the file, and what
    the line says after it. The first line is the file's finding, which the file names already,
    so the second line's own file name does not decide what it is about."""
    where = said.find(" " + about) + len(about)
    after = said[where:].lstrip(": ") if where > len(about) - 1 else ""
    return f"{about} that finding: {after[:LONGEST - len(about) - 17]}"


def summary(text: str) -> re.Match[str] | None:
    """A line that counts what a step found (`source file size: 2 problem(s); ...`) rather than
    naming what it is about: the findings above it say what it counts, and a problem too few
    keeps it out of what look like findings when one goes away."""
    return SUMMARY.match(text)


def worded(line: str) -> str:
    """The finding a line of a step's output reports in the step's own words, or "" for a blank
    one; its numbers blanked (see the module)."""
    text = " ".join(line.split())
    text = TEMPORARY.sub("/tmp/…", NUMBER.sub("N", text))
    for reserved in RESERVED:
        text = text.replace(reserved, " ")
    text = spaced(text)
    return text if len(text) <= LONGEST else text[:LONGEST - 1] + "…"


def finding(line: str) -> str:
    """The finding a line of a step's output reports, or "" for a blank one: the file and the rule
    it is about when it names a file of the tree, its own words otherwise (see the module)."""
    text = spaced(line)
    paths = [] if summary(text) else PATH.findall(text)
    if not paths:
        return worded(line)
    first, last = text.find(paths[0]), text.rfind(paths[-1])
    rule = (spaced(f"{text[:first]} {text[last + len(paths[-1]):]}") if first > 0 and last > first else "")
    if not rule:
        return paths[-1]
    return f"{paths[-1]} [{rule}]" if len(rule) <= LONGEST else f"{paths[-1]} [{rule[:LONGEST - 1]}…]"


def findings(output: str) -> list[tuple[str, str, str]]:
    """Each finding of `output` with its own words and the line it came from, keyed for t: about a
    file when the step named one, else as the step words it; a summary line only when the step
    named no file, since it counts findings that are already reported."""
    about: dict[str, list[str]] = {}
    summaries: list[tuple[str, str]] = []
    for line in output.splitlines():
        name = worded(line)
        if not name:
            continue
        if summary(line):
            summaries.append((name, line.rstrip()))
            continue
        about.setdefault(finding(line), []).append(line.rstrip())
    if not about:
        return [(name, line, name) for name, line in summaries]
    found = []
    for key, lines in about.items():
        repeats = len(lines) > 1
        for index, line in enumerate(lines):
            name = f"{key} ({index + 1})" if repeats and index else key
            if repeats and (match := REPEAT.match(key)):
                name = f"{more(worded(line), match['file'])} ({match['more']})"
            found.append((name, line, key))
    return found


def found_nothing(status: int, output: str) -> bool:
    """Whether the step failed without reporting problems: it crashed, was stopped, or was used
    wrongly (exit 2). Only exit 1 with no traceback is the step saying what it found."""
    return status != 1 or "Traceback (most recent call last)" in output


def report(found: list[tuple[str, str, str]]) -> str:
    """As libtest reports: each test's line, then each one's output, which ends at the next
    header or at the result line."""
    lines = ["", *(f"test {name} ... FAILED" for name, _, _ in found), "", "failures:", ""]
    for name, line, _ in found:
        lines += [f"---- {name} stdout ----", line, ""]
    lines += [f"test result: FAILED. 0 passed; {len(found)} failed", ""]
    return "\n".join(lines)


def quiet(found: list[tuple[str, str, str]]) -> str:
    """Each finding and the rule it is about, for a step whose own words do not say it."""
    lines = [f"  {name}: {rule}" for name, _, rule in found if rule != name]
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
            described = quiet(found)
            sys.stdout.write(f"\n{described}\n" if described else "")
            sys.stdout.write(report(found))
    sys.stdout.flush()
    return run.returncode if run.returncode >= 0 else 128 - run.returncode


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
