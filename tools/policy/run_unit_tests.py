#!/usr/bin/env python3
"""Run the unittest files of some directories side by side, a process each.

One `python3 -m unittest discover -s tools/tests` runs every test there in one process, one after
another: some 1,500 tests that mostly wait on Git and on the subprocesses they start, which took
the policy suite 8 to 30 minutes at a fraction of a core. Run a file per process, several at
once, and the suite takes about as long as its slowest file.

Each file runs as discover runs it (`-s DIR -p FILE`): DIR is the top of the import path, and the
file is imported as a module of that name, so a test passes or fails here as it does there.
Files share nothing but the machine: a test that needs a scratch directory makes its own.

At the end each file is reported as libtest reports a test, which t reads (docs/t.md): a file
that passed as `test <file> ... ok`, each test that failed in one as `test <file>::<test id> ...
FAILED` with the file's output under it, and a file that failed without naming a test (it
crashed, or did not import) as `test <file> ... FAILED`. t then compares each failing test with
main's, so a test that fails on main too passes and one more failing is refused.

Usage: run_unit_tests.py [--jobs N] DIR [DIR ...]
"""
from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass
from pathlib import Path
import re
import subprocess
import sys
import time

PATTERN = "test_*.py"
# The tests mostly wait, so more of them run at once than there are cores to give them.
DEFAULT_JOBS = 16
RAN = re.compile(r"^Ran (\d+) tests? in ", re.MULTILINE)
# unittest's header of a failed test: `FAIL: test_x (module.Class.test_x)`, before Python 3.11
# `FAIL: test_x (module.Class)`.
FAILED = re.compile(r"^(?:FAIL|ERROR|UNEXPECTED SUCCESS): (\S+) \(([^)\s]+)\)", re.MULTILINE)


@dataclass(frozen=True)
class Result:
    path: Path
    ok: bool
    seconds: float
    tests: int
    output: str


def files(directories: list[Path]) -> list[Path]:
    found = [path for directory in directories for path in sorted(directory.glob(PATTERN)) if path.is_file()]
    missing = [str(directory) for directory in directories if not directory.is_dir()]
    if missing:
        raise SystemExit(f"run_unit_tests.py: no such directory: {', '.join(missing)}")
    return found


def run(path: Path) -> Result:
    start = time.monotonic()
    done = subprocess.run(
        [sys.executable, "-m", "unittest", "discover", "-s", str(path.parent), "-p", path.name],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        errors="replace",
    )
    tests = sum(int(n) for n in RAN.findall(done.stdout))
    return Result(path, done.returncode == 0, time.monotonic() - start, tests, done.stdout)


def failed_tests(output: str) -> dict[str, str]:
    """The tests a file's unittest output says failed, by id in order, each with its report: its
    header and traceback, without unittest's rules of dashes (t ends a test's output at one)."""
    found: dict[str, str] = {}
    matches = list(FAILED.finditer(output))
    for index, match in enumerate(matches):
        name, where = match[1], match[2]
        test = where if where.endswith("." + name) else f"{where}.{name}"
        end = matches[index + 1].start() if index + 1 < len(matches) else len(output)
        block = [line for line in output[match.start():end].splitlines()
                 if line.strip("-=") or not line.strip()]
        found.setdefault(test, "\n".join(block).strip())
    return found


def report(results: list[Result]) -> str:
    """Each file and failed test in libtest's form (see the module's docstring)."""
    lines, sections = [""], []
    for result in sorted(results, key=lambda r: str(r.path)):
        if result.ok:
            lines.append(f"test {result.path} ... ok")
            continue
        tests = failed_tests(result.output)
        named = {f"{result.path}::{test}": text for test, text in tests.items()} or {
            str(result.path): "\n".join(line for line in result.output.splitlines() if line.strip("-=") or not line.strip())}
        for name, text in named.items():
            lines.append(f"test {name} ... FAILED")
            sections += [f"---- {name} stdout ----", text.rstrip(), ""]
    passed = sum(1 for r in results if r.ok)
    failed = len(lines) - 1 - passed
    return "\n".join([*lines, "", "failures:", "", *sections,
                      f"test result: {'FAILED' if failed else 'ok'}. {passed} passed; {failed} failed", ""])


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--jobs", type=int, default=DEFAULT_JOBS)
    parser.add_argument("directories", nargs="+", type=Path)
    args = parser.parse_args(argv)
    if args.jobs < 1:
        parser.error("--jobs must be at least 1")
    paths = files(args.directories)
    if not paths:
        print(f"run_unit_tests.py: no {PATTERN} in {', '.join(map(str, args.directories))}", file=sys.stderr)
        return 1

    start = time.monotonic()
    results: list[Result] = []
    with ThreadPoolExecutor(max_workers=args.jobs) as pool:
        for future in as_completed([pool.submit(run, path) for path in paths]):
            result = future.result()
            results.append(result)
            if not result.ok:
                print(f"\n---- {result.path} failed after {result.seconds:.1f}s:\n{result.output.rstrip()}\n", flush=True)

    failed = sorted(str(r.path) for r in results if not r.ok)
    print(report(results), flush=True)
    print("\nSlowest files:")
    for result in sorted(results, key=lambda r: r.seconds, reverse=True)[:10]:
        print(f"  {result.seconds:7.1f}s  {result.path}")
    print(f"\nRan {sum(r.tests for r in results)} tests in {len(results)} files in "
          f"{time.monotonic() - start:.1f}s, {args.jobs} at a time")
    if failed:
        print(f"{len(failed)} failed: {' '.join(failed)}")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
