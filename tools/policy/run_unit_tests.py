#!/usr/bin/env python3
"""Run the unittest files of some directories side by side, a process each.

One `python3 -m unittest discover -s tools/tests` runs every test there in one process, one after
another: some 1,500 tests that mostly wait on Git and on the subprocesses they start, which took
the policy suite 8 to 30 minutes at a fraction of a core. Run a file per process, several at
once, and the suite takes about as long as its slowest file.

Each file runs as discover runs it (`-s DIR -p FILE`): DIR is the top of the import path, and the
file is imported as a module of that name, so a test passes or fails here as it does there.
Files share nothing but the machine: a test that needs a scratch directory makes its own.

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
