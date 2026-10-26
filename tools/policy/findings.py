#!/usr/bin/env python3
"""Run a policy step and report what it found one finding at a time, as t compares tests.

  findings.py COMMAND [ARG...]

A step prints its findings on stdout and nothing else there, one per line, each
`path:line: message`, or `path: message` when no line applies. The path is the tree's file the
finding is about, relative to the top of the tree; a finding about something that is not a file
(a crate, a package) names the file that declares it, such as the `Cargo.toml` or `Cargo.lock`.
Everything else a step says, a heading, a count, a hint, a progress or OK line, goes to stderr,
so it is never counted, whatever its words.

Both streams are passed through to stdout, each chunk as it comes. When the step reports
problems (exit 1, no traceback), each finding is also reported as a failed test,
`test <finding> ... FAILED`, with its line under `---- <finding> stdout ----`: t compares a
check's failed tests with the same check's on main (docs/t.md), so a change passes a step that
main fails only when every finding it has, main has too, and a change that adds one is refused
even while main is red.

A finding is named by its path alone, never by its line or message, so a finding whose line
moved or whose count changed (`apps/b.rs: 3019 lines, over its ceiling of 3000` and later
`apps/b.rs: 3412 lines, over its ceiling of 3500`) is the same finding. A second finding about
one file is `apps/b.rs (2)`, so one more of what main has about a file is a finding main
doesn't have. A line on stdout that is not in the format is a finding all the same, named by its
words, so a misprinted one is never lost.

A step that crashed, was stopped, or failed without printing a finding found nothing: it fails
as a whole, compared with main as a whole.
"""
from __future__ import annotations

import os
import re
import selectors
import subprocess
import sys

# `path:line: message` or `path: message`: a path is one word with no colon in it.
FINDING = re.compile(r"^(?P<path>[^\s:]+)(?::(?P<line>\d+))?: (?P<message>\S.*)$")
# What t reads as a test's result line or as the header of its output: never part of a name.
RESERVED = (" ... ", "---- ", " stdout ----")


def about(line: str) -> str:
    """What a finding is about: its path, or a malformed line's own words."""
    if match := FINDING.match(line):
        return match["path"]
    text = " ".join(line.split())
    for reserved in RESERVED:
        text = text.replace(reserved, " ")
    return " ".join(text.split())


def findings(output: str) -> list[tuple[str, str]]:
    """Each finding of a step's stdout, named for t, with the line it came from."""
    seen: dict[str, int] = {}
    found = []
    for line in output.splitlines():
        line = line.rstrip()
        if not line.strip():
            continue
        key = about(line)
        seen[key] = seen.get(key, 0) + 1
        found.append((f"{key} ({seen[key]})" if seen[key] > 1 else key, line))
    return found


def found_nothing(status: int, said: str) -> bool:
    """Whether the step failed without reporting problems: it crashed, was stopped, or was used
    wrongly (exit 2). Only exit 1 with no traceback is the step saying what it found."""
    return status != 1 or "Traceback (most recent call last)" in said


def report(found: list[tuple[str, str]]) -> str:
    """As libtest reports: each test's line, then each one's output, which ends at the next
    header or at the result line."""
    lines = ["", *(f"test {name} ... FAILED" for name, _ in found), "", "failures:", ""]
    for name, line in found:
        lines += [f"---- {name} stdout ----", line, ""]
    lines += [f"test result: FAILED. 0 passed; {len(found)} failed", ""]
    return "\n".join(lines)


def run(argv: list[str]) -> tuple[int, str, str]:
    """Run the step, passing both its streams through to stdout as they come, and return its
    status, its stdout and its stderr. Python steps write unbuffered here, so a heading on stderr
    reads above the findings on stdout it introduces."""
    env = {**os.environ, "PYTHONUNBUFFERED": "1"}
    process = subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                               stdin=subprocess.DEVNULL, env=env)
    streams = {process.stdout: bytearray(), process.stderr: bytearray()}
    with selectors.DefaultSelector() as selector:
        for stream in streams:
            selector.register(stream, selectors.EVENT_READ)
        while selector.get_map():
            for key, _ in selector.select():
                chunk = os.read(key.fd, 65536)
                if not chunk:
                    selector.unregister(key.fileobj)
                    continue
                streams[key.fileobj] += chunk
                sys.stdout.buffer.write(chunk)
                sys.stdout.buffer.flush()
    status = process.wait()
    out, err = (bytes(streams[stream]).decode("utf-8", errors="replace")
                for stream in (process.stdout, process.stderr))
    return status, out, err


def main(argv: list[str]) -> int:
    if not argv:
        print(__doc__, file=sys.stderr)
        return 2
    status, out, err = run(argv)
    if status != 0 and not found_nothing(status, out + err):
        if found := findings(out):
            sys.stdout.write(report(found))
    sys.stdout.flush()
    return status if status >= 0 else 128 - status


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
